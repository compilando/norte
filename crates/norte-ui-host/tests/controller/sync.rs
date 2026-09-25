use super::*;

// ---------------------------------------------------------------------------
// Sync: the PLAN (task 6.3, phase A).
// ---------------------------------------------------------------------------

/// Waits for the next update carrying the sync panel.
pub(super) async fn next_sync(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Option<norte_ui_host::dto::SyncView> {
    for _ in 0..40 {
        let Ok(Some(u)) = tokio::time::timeout(WAIT_MAX, sub.recv()).await else {
            continue;
        };
        match u {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Sync { sync } = c {
                            return sync.clone();
                        }
                    }
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no update with the plan ever arrived");
}

/// A plan step, with the minimum needed to paint it.
pub(super) fn paso_de_plan(
    id: u64,
    rel: &str,
    kind: norte_proto::methods::SyncStepKind,
) -> norte_proto::methods::SyncStep {
    norte_proto::methods::SyncStep {
        id,
        kind,
        rel: norte_proto::methods::RelPath::new(
            rel.split('/')
                .map(|s| norte_proto::Segment::new(s.as_bytes().to_vec()).expect("segment"))
                .collect(),
        ),
        dest_rel: None,
        size: Some(10),
        criterion: norte_proto::methods::CompareCriterion::Size,
        confidence: norte_proto::methods::CompareConfidence::Certain,
        // The reversal that belongs to a copy: undoing it DELETES what it
        // created. The shared model rejects a step whose shape contradicts
        // itself — a class that writes with no reversal, a `Skip` that
        // claims to have one — and that rejection is what stops a plan that
        // cannot be painted from being approved.
        reversal: Some(norte_proto::methods::StepReversal::Delete),
        reason: None,
    }
}

/// A plan's closing, with no blockers.
pub(super) fn plan_closed(steps: u64) -> norte_proto::methods::SyncPlanDone {
    // The counts, as the daemon would count them: the model compares them
    // class by class against its own, and a plan that does not add up is
    // NOT approved. The bytes too: every step in this test measures ten.
    let counts = norte_proto::methods::SyncCounts {
        copy: steps,
        bytes: steps * 10,
        ..Default::default()
    };
    norte_proto::methods::SyncPlanDone {
        // Corrected on landing: the model matches the closing to ITS Task.
        task_id: norte_proto::TaskId::new(0),
        plan_hash: norte_proto::methods::PlanHash::parse(
            &"a".repeat(norte_proto::methods::PLAN_HASH_LEN),
        )
        .expect("test hash"),
        counts,
        blockers: Vec::new(),
        blockers_total: 0,
        executable: true,
        // With trash: it is what lets the undo column say something other
        // than "not known".
        dest_trash: norte_proto::methods::DestTrash::Restorable,
    }
}

/// Requesting a sync opens the panel with the plan the core answered, and
/// the plan says for each step whether undo brings it back.
#[tokio::test]
async fn requesting_sync_opens_the_plan() {
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((
        vec![
            // Both of the SAME class: the model compares the counts class
            // by class against the daemon's, and a plan that does not add
            // up is not approved — which is exactly what has to happen.
            paso_de_plan(1, "a.md", norte_proto::methods::SyncStepKind::Copy),
            paso_de_plan(2, "b.md", norte_proto::methods::SyncStepKind::Copy),
        ],
        plan_closed(2),
    ));
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;

    // Until the plan CLOSES: the steps arrive in one patch and the closing
    // in another, and what can be approved is a closed plan.
    let mut vista = next_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = next_sync(&mut sub).await.expect("still open");
    }
    assert_eq!(vista.steps.len(), 2, "{vista:?}");
    assert_eq!(vista.total, 2);
    // The mode is PAINTED before approving: a mirror deletes and an update
    // does not, and whoever approves has to see it.
    // The mode arrives already TRANSLATED through the shared label, not as
    // an id: a mode this build did not know how to name cannot fall back to
    // "update", which is the safe half of what is being approved.
    assert_eq!(
        vista.mode,
        norte_frontend::sync::mode_label(
            norte_proto::methods::SyncMode::Update,
            norte_i18n::Lang::Es
        )
    );
    // And every step says whether undo returns it: it never comes out of
    // `reversal` plain and simple, which is the half that lies with no
    // trash bin at the destination.
    assert!(vista.steps.iter().all(|p| !p.undo.is_empty()), "{vista:?}");
    assert!(
        vista.can_approve,
        "a closed plan with no blockers gets approved: {}",
        vista.status
    );
    let requests = backend.planes_requests.lock().expect("plans").clone();
    assert_eq!(requests.len(), 1);
    assert_ne!(
        requests[0].0, requests[0].1,
        "source and destination are different"
    );
}

/// A plan with BLOCKERS cannot be approved, and it says which ones they are.
#[tokio::test]
async fn a_plan_with_blockers_is_not_approved() {
    let mut done = plan_closed(1);
    done.blockers = vec![norte_proto::methods::SyncBlocker {
        kind: norte_proto::methods::SyncBlockerKind::DestReadOnly,
        // The root: a lock on the whole tree hangs off no step.
        rel: norte_proto::methods::RelPath::new(Vec::new()),
        side: None,
    }];
    done.executable = false;
    done.blockers_total = 1;
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        done,
    ));
    let (h, _snap) = host_con_layout(Arc::new(fake), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;

    let mut vista = next_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if !vista.blockers.is_empty() {
            break;
        }
        vista = next_sync(&mut sub).await.expect("still open");
    }
    assert!(
        !vista.blockers.is_empty(),
        "it says what is stopping it: {vista:?}"
    );
    assert!(
        !vista.can_approve,
        "and approving is not offered: {vista:?}"
    );
}

/// Syncing both panes when they are in the SAME place queues nothing.
#[tokio::test]
async fn syncing_the_same_directory_queues_nothing() {
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((Vec::new(), plan_closed(0)));
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    // Without separating the panes: both look at `home`. And the command
    // really RUNS — the previous version of this test pressed `Escape` on
    // the palette and asserted that nothing had been requested, which is
    // true whether the guard exists or not.
    let ack = execute_via_palette_ack(&h, &mut sub, "pane.sync-dirs").await;
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-same-directory".to_owned()
        },
        "{ack:?}"
    );
    assert!(
        backend.planes_requests.lock().expect("planes").is_empty(),
        "no plan was requested"
    );
}

/// Cancelling the plan's Task from the BOARD leaves the panel saying it was
/// cancelled, not "planning…" forever.
///
/// The Task's outcome was not reaching the model, so `run` was left in
/// `Running` forever: the panel did not know how to say "cancelled" nor
/// "failed", and — what matters for the next phase — kept saying the plan
/// can be approved after someone told it to stop.
#[tokio::test]
async fn cancelling_the_plan_from_the_dashboard_says_so_in_the_pane() {
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_closed(1),
    ));
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;
    let vista = next_sync(&mut sub).await.expect("opens");
    assert!(vista.running);

    // The daemon says the Task was cancelled.
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Cancelled);

    for _ in 0..40 {
        let v = next_sync(&mut sub).await.expect("still open");
        if !v.running {
            assert!(
                !v.can_approve,
                "a cancelled plan is not approved, whether it closed or not: {v:?}"
            );
            return;
        }
    }
    panic!("the panel kept saying it is planning");
}

/// With the plan panel up front, another one cannot be requested.
///
/// Relaunching left the previous panel without leaving and its Task
/// uncancelled — the daemon kept walking a tree for a plan nobody can see
/// anymore — and, with a request in flight, the second keystroke was
/// killing both panels.
#[tokio::test]
async fn with_the_plan_pane_in_front_no_other_is_requested() {
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_closed(1),
    ));
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;
    let _ = next_sync(&mut sub).await.expect("opens");

    // With the panel up front, the keys are ITS OWN: `ctrl+p` does not open
    // the palette, which is the path through which the command would be
    // repeated. It is the first of the two locks.
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(
        snapshot.palette.is_none(),
        "the plan panel cannot let the palette's key through"
    );
    assert!(snapshot.sync.is_some(), "and the panel is still up front");
    assert_eq!(
        backend.planes_requests.lock().expect("planes").len(),
        1,
        "no second plan was requested"
    );
}

/// A daemon that does not know how to plan does not leave the request
/// hanging.
#[tokio::test]
async fn a_plan_the_daemon_rejects_leaves_nothing_pending() {
    let fake = tree_as_fake();
    // No plan: the fake answers `Unsupported`.
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;
    // The failure is SAID.
    for _ in 0..40 {
        if next_notice(&mut sub).await.starts_with("err-") {
            break;
        }
    }
    // And the next attempt can be made: the request did not get stuck.
    let ack = execute_via_palette_ack(&h, &mut sub, "pane.sync-dirs").await;
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the previous request left the host stuck: {ack:?}"
    );
}

/// A plan that DELETES trees asks TWICE, and the second is only answered
/// with `y`.
///
/// The second question is not ceremony: it is composed by the shared model
/// and only appears when the plan deletes or leaves something with no way
/// back. Always asking is what teaches people to answer without reading.
#[tokio::test]
async fn a_plan_that_deletes_asks_twice() {
    let mut done = plan_closed(1);
    done.counts = norte_proto::methods::SyncCounts {
        delete_tree: 1,
        ..Default::default()
    };
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((
        vec![norte_proto::methods::SyncStep {
            reversal: Some(norte_proto::methods::StepReversal::RestoreTrash),
            ..paso_de_plan(1, "viejo", norte_proto::methods::SyncStepKind::DeleteTree)
        }],
        done,
    ));
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;
    let mut vista = next_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = next_sync(&mut sub).await.expect("still open");
    }
    assert!(vista.can_approve, "{}", vista.status);

    // The first `a` only ASKS.
    // TODO(translation): review — the comment says `a`, but the dispatched
    // key below is "y"; kept as in the source.
    h.dispatch(press("y")).await.expect("host alive");
    let asking = next_sync(&mut sub).await.expect("still open");
    assert!(
        asking.confirming.is_some(),
        "a plan that deletes trees asks again: {asking:?}"
    );
    settle().await;
    assert!(
        backend.applied.lock().expect("aplicados").is_empty(),
        "and it has still applied nothing"
    );

    // A key that is not `y` WITHDRAWS the question and applies nothing.
    h.dispatch(press("n")).await.expect("host alive");
    let withdrawn = next_sync(&mut sub).await.expect("still open");
    assert!(withdrawn.confirming.is_none());
    settle().await;
    assert!(backend.applied.lock().expect("aplicados").is_empty());

    // `a` and then `y`: now it does, and with the hash the CORE returned.
    h.dispatch(press("y")).await.expect("host alive");
    let _ = next_sync(&mut sub).await;
    h.dispatch(press("y")).await.expect("host alive");
    let applied = annotated(&backend, "the plan applied", 1, |f| {
        f.applied.lock().expect("aplicados").clone()
    })
    .await;
    assert_eq!(applied.len(), 1, "only once");
}

/// A REJECTED apply releases the latch; one with an UNKNOWN outcome does
/// not.
///
/// The daemon answering "no" and the connection dropping after asking are
/// different things: in the first case the destination is known to be
/// intact and retrying is correct; in the second the request may have
/// arrived, and offering `y` again is offering to apply the same plan twice
/// over the same destination.
#[tokio::test]
async fn an_apply_of_an_unknown_result_is_not_reoffered() {
    for (error, is_offered_again) in [
        (
            norte_proto::Error::PolicyDenied {
                rule: "policy-rule".to_owned(),
            },
            true,
        ),
        (norte_proto::Error::Io { retryable: true }, false),
    ] {
        let fake = tree_as_fake();
        *fake.plan_de_sync.lock().expect("plan") = Some((
            vec![paso_de_plan(
                1,
                "a.md",
                norte_proto::methods::SyncStepKind::Copy,
            )],
            plan_closed(1),
        ));
        *fake.error_on_apply.lock().expect("error") = Some(error.clone());
        let backend = Arc::new(fake);
        let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
        let mut sub = h.subscribe();
        separate_the_panes(&h, &mut sub).await;
        run_by_palette(&h, &mut sub, "pane.sync-dirs").await;
        let mut vista = next_sync(&mut sub).await.expect("opens");
        for _ in 0..20 {
            if vista.can_approve {
                break;
            }
            vista = next_sync(&mut sub).await.expect("still open");
        }
        assert!(vista.can_approve, "{}", vista.status);

        h.dispatch(press("y")).await.expect("host alive");
        annotated(&backend, "the apply requested", 1, |f| {
            f.applied.lock().expect("aplicados").clone()
        })
        .await;
        // The apply's outcome comes back through the mailbox: it is let run
        // before asking what the screen shows.
        settle().await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let then = next_snapshot(&mut sub).await.sync.expect("still open");
        assert_eq!(
            then.can_approve, is_offered_again,
            "{error:?} left the screen offering approve = {}",
            then.can_approve
        );
    }
}

/// With the apply IN FLIGHT, `Escape` asks to cancel and does NOT close the
/// panel.
///
/// Closing it loses the report — and with it the count, the failures and
/// the undo handle — over a destination that is being rewritten.
#[tokio::test]
async fn with_apply_in_flight_escape_does_not_close() {
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_closed(1),
    ));
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;
    let mut vista = next_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = next_sync(&mut sub).await.expect("still open");
    }
    // This plan deletes nothing and undoes entirely: there is no second
    // question. `y` is `dialog.approve` in the preset (#287): approving a
    // plan is saying yes to what is already up front, not a bare "confirm".
    h.dispatch(press("y")).await.expect("host alive");
    annotated(&backend, "the plan applied", 1, |f| {
        f.applied.lock().expect("aplicados").clone()
    })
    .await;
    assert_eq!(backend.applied.lock().expect("aplicados").len(), 1);

    // The FIRST `Escape` asks to stop and does NOT close: closing loses the
    // report on a destination half rewritten. And it asks to stop the
    // APPLY's task, not the plan's, which finished a while ago.
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    let panel = snapshot.sync.expect("the panel stays");
    assert!(
        panel.cancel_requested,
        "and the screen acknowledges it was heard"
    );
    until(&backend, "the stop requested from the daemon", |f| {
        (!f.canceled_by_id.lock().expect("canceladas").is_empty()).then_some(())
    })
    .await;
    let stops = backend.canceled_by_id.lock().expect("canceladas").clone();
    assert!(
        stops.iter().any(|id| *id >= 500),
        "the apply's task was asked to stop: {stops:?}"
    );

    // The SECOND one closes, whatever happens with the report: without this
    // exit, the writing screen was the only one in norte with no exit.
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    for _ in 0..20 {
        if next_snapshot(&mut sub).await.sync.is_none() {
            return;
        }
    }
    panic!("the panel could not be closed");
}

/// The report arrives and the panel says so, with the failures one by one.
#[tokio::test]
async fn the_sync_report_says_what_failed() {
    let fake = tree_as_fake();
    *fake.plan_de_sync.lock().expect("plan") = Some((
        vec![paso_de_plan(
            1,
            "a.md",
            norte_proto::methods::SyncStepKind::Copy,
        )],
        plan_closed(1),
    ));
    *fake.sync_report.lock().expect("informe") = Some(norte_proto::methods::SyncReportResult {
        done: 0,
        failed: 1,
        skipped: 0,
        bytes: 0,
        failures: vec![norte_proto::methods::SyncFailure {
            rel: norte_proto::methods::RelPath::new(vec![
                norte_proto::Segment::new(b"a.md".to_vec()).expect("segmento"),
            ]),
            dest_rel: None,
            cause: norte_proto::methods::SyncFailureCause::Denied,
            kind: norte_proto::methods::SyncStepKind::Copy,
        }],
        // With no journal batch: nothing to undo, and the panel will
        // say so.
        batch_id: None,
        dest_trash: norte_proto::methods::DestTrash::Restorable,
    });
    let backend = Arc::new(fake);
    let (h, _snap) = host_con_layout(Arc::clone(&backend), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    separate_the_panes(&h, &mut sub).await;
    run_by_palette(&h, &mut sub, "pane.sync-dirs").await;
    let mut vista = next_sync(&mut sub).await.expect("opens");
    for _ in 0..20 {
        if vista.can_approve {
            break;
        }
        vista = next_sync(&mut sub).await.expect("still open");
    }
    h.dispatch(press("y")).await.expect("host alive");
    annotated(&backend, "the plan applied", 1, |f| {
        f.applied.lock().expect("aplicados").clone()
    })
    .await;
    // The daemon finishes the apply's Task.
    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    for _ in 0..40 {
        let v = next_sync(&mut sub).await.expect("still open");
        if !v.failures.is_empty() {
            assert_eq!(v.failures[0].path, "a.md");
            assert!(!v.failures[0].cause.is_empty());
            return;
        }
    }
    panic!("the report never reached the panel");
}

/// Approving an extension's capabilities ASKS, and the question enumerates
/// them.
///
/// "Do you approve org.example.foo?" without saying what it grants is not a
/// decision: it is a button. Each capability goes on its own LINE and with
/// its own flag, because the one painted differently from what it says is
/// exactly the one a hostile manifest writes to sneak in among the real
/// ones.
#[tokio::test]
async fn approving_asks_and_lists_the_capabilities() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    ext.capabilities = vec!["fs-read".to_owned(), "net\u{202e}".to_owned()];
    let backend = tree_with_plugins(vec![ext], &[]);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = extensions_loaded(&mut sub).await;

    h.dispatch(press("a")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the question");
    assert_eq!(d.title_key, "modal-extension-approve-title");
    assert_eq!(d.body.len(), 3, "the name and the TWO capabilities: {d:?}");
    assert!(!d.body[1].hostile, "the clean capability is not marked");
    assert!(
        d.body[2].hostile,
        "and the one with the bidi override IS: which one differs is the whole question"
    );
    // And who is asking, by the id the core validates: two extensions can
    // share a name, and the name is written by the manifest.
    assert_eq!(
        d.subject.as_ref().map(|s| s.text.as_str()),
        Some("acme.ftp")
    );
    settle().await;
    assert!(
        backend.governance.lock().expect("gobierno").is_empty(),
        "and nothing has been granted yet"
    );

    // And the affirmative response grants, and the catalogue is
    // RE-REQUESTED: what the screen says about who can read your files is
    // not decided by local optimism.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    for _ in 0..20 {
        let Some(v) = next_extensions(&mut sub).await else {
            continue;
        };
        if v.rows.first().is_some_and(|r| r.approved) {
            assert_eq!(
                backend.governance.lock().expect("gobierno").as_slice(),
                // With the ANCHOR that was shown (#282): what gets granted
                // has to be what the human read, and the core refuses if
                // the manifest changed between the question and the yes.
                ["approval:acme.ftp:true:digest-de-acme.ftp"]
            );
            return;
        }
    }
    panic!("the catalogue never reflected the grant");
}

/// In read-only nothing is granted: it is SAID.
///
/// It is the same switch that decides whether this window deletes. Granting
/// capabilities is the extension system's security decision, and a window
/// mounted with no effects does not make it.
#[tokio::test]
async fn in_read_only_no_extension_is_governed() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    let backend = tree_with_plugins(vec![ext], &[]);
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = extensions_loaded(&mut sub).await;
    for key_for in ["a", "e", "d"] {
        let ack = h.dispatch(press(key_for)).await.expect("host alive");
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
            "`{key_for}` in read-only: {ack:?}"
        );
    }
    // And the button (bridge 61) goes through the same door as the key.
    let ack = h
        .dispatch(UiAction::ExtensionGovern {
            row: 0,
            id: "acme.ftp".to_owned(),
            change: norte_ui_host::action::ExtensionChange::Uninstall,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
        "uninstalling by button in read-only: {ack:?}"
    );
    settle().await;
    assert!(backend.governance.lock().expect("gobierno").is_empty());
}

/// Enabling an extension WITHOUT approving it is refused, and it says why.
///
/// Without approved capabilities the core does not load it: saying
/// "enabled" about something that is not running is the screen lying.
#[tokio::test]
async fn turning_on_without_approving_is_refused() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    let backend = tree_with_plugins(vec![ext], &[]);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = extensions_loaded(&mut sub).await;
    let ack = h.dispatch(press("e")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "host-extension-not-approved"),
        "{ack:?}"
    );
    settle().await;
    assert!(backend.governance.lock().expect("gobierno").is_empty());
}

/// A `bool` CYCLES with `Enter` and gets written; an `int` opens the buffer,
/// and what is typed is validated against the SCHEMA's bounds before
/// leaving.
#[tokio::test]
async fn the_config_editor_cycles_types_and_validates() {
    let ext = extension("acme.ftp", "FTP de ACME", false);
    let backend = tree_with_scheme(vec![ext], &[("acme.ftp", test_scheme())]);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = extensions_loaded(&mut sub).await;
    h.dispatch(press("Enter")).await.expect("host alive");
    let detail = detail_open(&mut sub).await;
    assert_eq!(detail.config.len(), 4);
    assert!(
        !detail.config[3].editable,
        "a `kind` this build does not know is read-only: {:?}",
        detail.config[3]
    );

    // The first key is the `bool`: `Enter` cycles it and sends it. It WAITS
    // for it to arrive instead of sleeping a fixed span: under load, forty
    // milliseconds are no guarantee, and a test that asserts presence
    // against the clock is intermittently red.
    h.dispatch(press("Enter")).await.expect("host alive");
    annotated(&backend, "the `bool`'s write", 1, |f| {
        f.writes.lock().expect("escrituras").clone()
    })
    .await;
    assert_eq!(
        backend.writes.lock().expect("escrituras").as_slice(),
        [(
            "acme.ftp".to_owned(),
            "verbose".to_owned(),
            "true".to_owned()
        )]
    );
    // And the screen moves with it: the operand and what is painted are two
    // halves of the same row, and updating only one left the cell with the
    // old value — the next `Enter` would send it back to where it was.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let detail = next_snapshot(&mut sub)
        .await
        .extensions
        .expect("still open")
        .detail
        .expect("with a card");
    assert_eq!(detail.config[0].value, "true");

    // The second one is the `int`: `Enter` opens the buffer and writes
    // NOTHING.
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    h.dispatch(press("Enter")).await.expect("host alive");
    wait_buffer(&h, &mut sub).await;
    // A value outside the bounds is refused HERE and does not travel: the
    // daemon validates again, but saying it beforehand saves the trip and
    // states the bound.
    for c in ["Backspace", "Backspace", "9", "9", "9"] {
        h.dispatch(press(c)).await.expect("host alive");
    }
    let ack = h.dispatch(press("Enter")).await.expect("host alive");
    // The ACK carries a key with no variables — nobody substitutes
    // `{ $min }` on that path — the bounds go in the notice, which does
    // get translated with them.
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "host-value-rejected"),
        "{ack:?}"
    );
    settle().await;
    assert_eq!(
        backend.writes.lock().expect("escrituras").len(),
        1,
        "the out-of-range value was not sent"
    );
    // And the buffer STAYS open: a rejected commit does not close the
    // field, which is what lets someone correct it without retyping it
    // whole.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        next_snapshot(&mut sub)
            .await
            .extensions
            .expect("still open")
            .detail
            .expect("with a card")
            .editing
            .is_some()
    );

    // And one inside the bounds does.
    h.dispatch(press("Enter")).await.expect("host alive");
    wait_buffer(&h, &mut sub).await;
    for c in ["Backspace", "Backspace", "Backspace", "4", "2"] {
        h.dispatch(press(c)).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");
    let writes = annotated(&backend, "the typed key, sent", 2, |f| {
        f.writes.lock().expect("escrituras").clone()
    })
    .await;
    assert_eq!(writes[1].1, "timeout");
    assert_eq!(writes[1].2, "42");
}

/// While a value is being TYPED, `a` is a letter, not a grant.
///
/// It is the same fixed regime as any field in this host: resolving letters
/// as gestures there would turn typing "casa" into two capability grants.
#[tokio::test]
async fn while_typing_a_value_letters_are_letters() {
    let ext = extension("acme.ftp", "FTP de ACME", false);
    let backend = tree_with_scheme(vec![ext], &[("acme.ftp", test_scheme())]);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = extensions_loaded(&mut sub).await;
    h.dispatch(press("Enter")).await.expect("host alive");
    let _ = detail_open(&mut sub).await;
    // To the `string` key, which is the THIRD one (`verbose`, `timeout`,
    // `greeting`, and the fourth is the unknown `kind`'s).
    for _ in 0..2 {
        h.dispatch(press("ArrowDown")).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");
    wait_buffer(&h, &mut sub).await;
    h.dispatch(press("a")).await.expect("host alive");
    settle().await;
    assert!(
        backend.governance.lock().expect("gobierno").is_empty(),
        "the typed `a` granted no capabilities"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    let editing = snapshot
        .extensions
        .expect("still open")
        .detail
        .expect("with a card")
        .editing
        .expect("editing");
    assert!(editing.ends_with('a'), "the letter went in: {editing:?}");
}

/// A tree with a catalogue AND `[config]` schemas.
pub(super) fn tree_with_scheme(
    plugins: Vec<norte_proto::methods::PluginInfo>,
    schemes: &[(&str, Vec<norte_proto::methods::PluginConfigKeyWire>)],
) -> Arc<Fake> {
    let base = fake_tree();
    let mut f = Fake {
        plugins: plugins.into(),
        schemes: schemes
            .iter()
            .map(|(id, keys)| ((*id).to_owned(), keys.clone()))
            .collect(),
        ..Fake::default()
    };
    f.tree.clone_from(&base.tree);
    Arc::new(f)
}

/// The test schema: a `bool`, a bounded `int` and a `kind` this build does
/// not know.
pub(super) fn test_scheme() -> Vec<norte_proto::methods::PluginConfigKeyWire> {
    vec![
        norte_proto::methods::PluginConfigKeyWire {
            key: "verbose".to_owned(),
            kind: "bool".to_owned(),
            default: "false".to_owned(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "false".to_owned(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "timeout".to_owned(),
            kind: "int".to_owned(),
            default: "10".to_owned(),
            min: Some(1),
            max: Some(300),
            values: Vec::new(),
            description: None,
            value: "30".to_owned(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "greeting".to_owned(),
            kind: "string".to_owned(),
            default: "hola".to_owned(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "hola".to_owned(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "future".to_owned(),
            kind: "duration".to_owned(),
            default: "1s".to_owned(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "1s".to_owned(),
        },
    ]
}

/// Waits for the chosen extension's card to be open.
pub(super) async fn detail_open(
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::ExtensionDetailView {
    for _ in 0..20 {
        let Some(v) = next_extensions(sub).await else {
            continue;
        };
        if let Some(d) = v.detail {
            return d;
        }
    }
    panic!("the card never opened");
}

/// The palette offers extension commands, and running them shows what they
/// printed.
///
/// The rows are composed by the SHARED model: only approved and enabled
/// ones — the same gate `plugin.run_command` enforces on its own — and with
/// the prefix that stops a third party's command from disguising itself as
/// one of the host's own. The output is third-party text: it gets masked,
/// capped, and it says when it was cut off.
#[tokio::test]
async fn the_palette_runs_an_extension_command_and_shows_its_output() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "greet".to_owned(),
        title: "Saludar".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Command,
    }];
    let backend = tree_with_plugins(vec![ext], &[]);
    *backend.command_output.lock().expect("salida") =
        Some(Ok(format!("hola\u{202e}{}", "x".repeat(5_000))));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    // Plugin rows are MERGED IN when the daemon answers: the palette is
    // painted first, with the host's own commands.
    let mut got = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = next_snapshot(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Saludar")) {
            got = true;
            break;
        }
    }
    assert!(got, "the extension command's row never arrived");
    // It gets narrowed by typing, which is what the palette is for: the
    // command's title is folded by the shared model together with its
    // description.
    for c in "Saludar".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let p = next_snapshot(&mut sub).await.palette.expect("open");
    assert_eq!(p.rows.len(), 1, "the filter leaves a single row: {p:?}");
    h.dispatch(press("Enter")).await.expect("host alive");

    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snapshot = next_snapshot(&mut sub).await;
        let Some(output) = snapshot.plugin_output else {
            continue;
        };
        assert_eq!(
            backend.executed.lock().expect("ejecutados").as_slice(),
            [("acme.ftp".to_owned(), "greet".to_owned())]
        );
        assert!(output.text_hostile, "the bidi override is said: {output:?}");
        assert!(
            !output.lines.iter().any(|l| l.contains('\u{202e}')),
            "and it is masked"
        );
        assert!(output.truncated, "and that it was cut off, too: {output:?}");
        assert_eq!(output.command.text, "Saludar");
        assert_eq!(output.plugin_id, "acme.ftp", "and who printed it, by id");

        // And `Escape` closes it without touching anything underneath.
        h.dispatch(press("Escape")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        assert!(next_snapshot(&mut sub).await.plugin_output.is_none());
        return;
    }
    panic!(
        "the command's output never arrived; ejecutados = {:?}",
        backend.executed.lock().expect("ejecutados")
    );
}

/// C3 (ADR 0095): a RENAMER row in the palette asks the plugin for a plan
/// over what is marked and puts it into the SAME review as the AI's plan —
/// with the core's verdict along for the ride — with no model involved.
#[tokio::test]
async fn the_palette_requests_the_plan_from_a_renamer_and_reviews_it_like_the_ais() {
    let pares = [("ep1.mkv", "2026-09-03_ep1.mkv")];
    let mut ext = extension("org.norte.date-prefix", "Date prefix", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "by-date".to_owned(),
        title: "Rename by date".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Renamer,
    }];
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        pares
            .iter()
            .map(|(from, _)| (from.as_bytes().to_vec(), false))
            .collect::<Vec<_>>(),
    );
    f.plan_renamer = Some(
        pares
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    );
    f.verdict = Some(verdict_ok(&pares));
    *f.plugins.lock().expect("plugins") = vec![ext];
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Marks the file: the renamer acts on what is marked.
    by_palette(&h, &mut sub, "mark.all").await;
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let mut arrived = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = next_snapshot(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Rename by date")) {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the renamer's row never arrived");
    for c in "Rename by date".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let p = next_snapshot(&mut sub).await.palette.expect("open");
    assert_eq!(p.rows.len(), 1, "{p:?}");
    // The label comes from the process's GLOBAL catalogue (like the
    // extension commands' does), so either one is valid here.
    assert!(
        p.rows[0].text.starts_with("[renombrar]") || p.rows[0].text.starts_with("[rename]"),
        "a different label than a command's: {}",
        p.rows[0].text
    );
    h.dispatch(press("Enter")).await.expect("host alive");

    let mut v = next_revision(&mut sub).await.expect("the review opens");
    assert_eq!(v.pairs[0].from.text, "ep1.mkv");
    assert_eq!(v.pairs[0].to.text, "2026-09-03_ep1.mkv");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = next_revision(&mut sub).await.expect("still open");
    }
    assert!(v.confirmable, "the core gave its verdict");
    let requests = backend.renamers_requests.lock().expect("mutex").clone();
    assert_eq!(
        requests,
        vec![(
            "org.norte.date-prefix".to_owned(),
            "by-date".to_owned(),
            vec!["ep1.mkv".to_owned()]
        )]
    );
    assert!(
        backend.instructions.lock().expect("mutex").is_empty(),
        "the model was not asked for anything"
    );
}

/// A renamer that REFUSES says why (#332): the phrase reaches the status
/// bar exactly as the daemon capped it, no review opens, and it is not a
/// generic error — "approve my capability" has to be readable.
#[tokio::test]
async fn a_renamer_that_refuses_says_why_in_the_bar() {
    let mut ext = extension("org.norte.date-prefix", "Date prefix", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "by-date".to_owned(),
        title: "Rename by date".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Renamer,
    }];
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.renamer_refuses = Some("needs the location capability".to_owned());
    *f.plugins.lock().expect("plugins") = vec![ext];
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    by_palette(&h, &mut sub, "mark.all").await;
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let mut arrived = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = next_snapshot(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Rename by date")) {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the renamer's row never arrived");
    for c in "Rename by date".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");

    // The same snapshot carries the phrase and the absence of a review:
    // waiting for ANOTHER snapshot afterward would hang, because nothing
    // else changes.
    let (msg, revision) = snapshot_until(&h, &mut sub, "the renamer's phrase in the bar", |s| {
        s.status
            .message
            .clone()
            .filter(|m| m.contains("needs the location capability"))
            .map(|m| (m, s.ai_rename.is_some()))
    })
    .await;
    assert!(
        !msg.contains("no soportado") && !msg.contains("not supported"),
        "it is not a generic error: {msg}"
    );
    assert!(!revision, "with no plan there is no review");
}

/// In read-only the palette does NOT offer extension commands.
///
/// What a command does is decided by the PLUGIN: it can write. A window
/// mounted with no effects does not launch it, and therefore does not offer
/// it either — it is the same rule already applied to the host's own
/// commands: offering what is going to be refused is promising something
/// that will not happen.
///
/// What IS requested is the catalogue (phase 3): it carries the declaration
/// of what panels the plugins contribute, and without it a saved layout
/// with a panel leaves a blank box the reader cannot identify. Requesting it
/// is not offering it — what this test pins down is that it is neither
/// offered nor launched.
#[tokio::test]
async fn in_read_only_an_extension_command_does_not_run() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "greet".to_owned(),
        title: "Saludar".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Command,
    }];
    let backend = tree_with_plugins(vec![ext], &[]);
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = next_snapshot(&mut sub).await.palette.expect("open");
        assert!(
            !p.rows.iter().any(|r| r.text.contains("Saludar")),
            "a window with no effects does not offer to run third-party code"
        );
        settle().await;
    }
    // And nothing ran, which is what the rule protects. The catalogue does
    // travel — `request_panels` requests it to declare kinds, even with no
    // effects — so counting round trips stopped saying anything about what
    // is offered.
    assert!(backend.executed.lock().expect("ejecutados").is_empty());
}

/// Waits for the card's editing buffer to be open.
///
/// By RESYNC and not by blindly consuming patches: a loop that reads N
/// updates runs out of them as soon as the test sends a snapshot for
/// another reason, and then fails on a deadline saying something that is
/// not true.
pub(super) async fn wait_buffer(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let open = next_snapshot(sub)
            .await
            .extensions
            .and_then(|e| e.detail)
            .is_some_and(|d| d.editing.is_some());
        if open {
            return;
        }
    }
    panic!("the editing buffer never opened");
}

/// A governance change that FAILS asks for the catalogue again.
///
/// The failure carries THIS side's deadline, which is not "it did not
/// happen" but "it is not known": the daemon may have granted the
/// capabilities and taken a while to answer. Leaving the row saying "not
/// approved" is the same lie as local optimism, in pessimistic form — and
/// the only thing that resolves an unknown is asking.
#[tokio::test]
async fn a_failed_governance_check_asks_the_core_again() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.approved = false;
    ext.enabled = false;
    ext.capabilities = vec!["fs-read".to_owned()];
    let backend = tree_with_plugins(vec![ext], &[]);
    *backend.governance_error.lock().expect("gobierno") =
        Some(norte_proto::Error::ProviderUnavailable { retryable: true });
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(press("F12")).await.expect("host alive");
    let _ = extensions_loaded(&mut sub).await;
    let requests = backend
        .catalogos_requests
        .load(std::sync::atomic::Ordering::SeqCst);

    h.dispatch(press("a")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the question");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "approve".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");

    until(
        &backend,
        "the catalogue re-requested after governance",
        |f| {
            let now = f
                .catalogos_requests
                .load(std::sync::atomic::Ordering::SeqCst);
            (now > requests).then_some(())
        },
    )
    .await;
}

/// With a command's output on screen, the keys are ITS OWN.
///
/// It paints full-screen, so a modal that let through a key it does not
/// understand is not a modal: `Enter` on that panel was reaching what was
/// underneath, where a confirmation could be waiting for a yes the reader
/// does not see — and the moment is chosen by the PLUGIN, which decides
/// when it answers.
#[tokio::test]
async fn a_commands_output_does_not_let_keys_through() {
    let mut ext = extension("acme.ftp", "FTP de ACME", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "greet".to_owned(),
        title: "Saludar".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Command,
    }];
    let backend = tree_with_plugins(vec![ext], &[]);
    *backend.command_output.lock().expect("salida") = Some(Ok("hola".to_owned()));
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let before = next_snapshot_after_resync(&h, &mut sub).await;
    let cursor_before = listing(&before).cursor;

    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = next_snapshot(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Saludar")) {
            break;
        }
    }
    for c in "Saludar".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if next_snapshot(&mut sub).await.plugin_output.is_some() {
            break;
        }
    }

    // A navigation key with the panel open does NOT move what is
    // underneath.
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    let during = next_snapshot_after_resync(&h, &mut sub).await;
    assert!(during.plugin_output.is_some(), "the panel stays");
    assert_eq!(
        listing(&during).cursor,
        cursor_before,
        "the listing's cursor did not move underneath the panel"
    );

    // And `Enter` CLOSES it, which is the reflex of someone who just read
    // it.
    h.dispatch(press("Enter")).await.expect("host alive");
    let after = next_snapshot_after_resync(&h, &mut sub).await;
    assert!(after.plugin_output.is_none());
    assert_eq!(listing(&after).cursor, cursor_before);
}

/// Requests a snapshot and waits for it.
pub(super) async fn next_snapshot_after_resync(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::ViewSnapshot {
    h.dispatch(UiAction::Resync).await.expect("host alive");
    next_snapshot(sub).await
}

/// The agents panel lists the sessions THIS window saw ask for permission,
/// and one of them gets undone whole from there (#276).
///
/// The operand is CHOSEN from a list: a session id typed by hand on a
/// governance surface is an id that can be gotten wrong, and undoing the
/// wrong session is undoing someone else's work. And the list says what it
/// is — what this window has seen, not the system's census — because there
/// is no protocol method that enumerates live sessions.
#[tokio::test]
async fn the_agents_pane_undoes_the_chosen_session() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Two sessions, and the one with the hostile id is the LAST seen: the
    // list goes from most recent to oldest.
    for (id, op, aid) in [
        ("agente-2", "copy", 11_u64),
        ("agente\u{202e}1", "delete", 12),
    ] {
        tx.send(norte_proto::methods::PolicyApprovalRequired {
            approval_id: aid,
            session: Some(id.to_owned()),
            op: op.to_owned(),
            paths: vec!["mem:///casa/x".to_owned()],
            paths_total: 1,
            ttl_ms: 30_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        })
        .expect("the host is listening");
        let dialogs = next_dialogs(&mut sub).await;
        // It is DENIED to get it out of the way: an open dialog keeps the
        // keys, and what is checked here is the panel. Denying does not
        // erase the record — what the session asked for has already been
        // seen — which is exactly the interesting property.
        let d = dialogs.last().expect("the approval");
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
        let _ = next_dialogs(&mut sub).await;
    }

    run_by_palette(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let panel = next_snapshot(&mut sub)
        .await
        .agents
        .expect("the panel is open");
    assert_eq!(panel.rows.len(), 2);
    assert_eq!(panel.rows[0].last_op, "delete", "the most recent first");
    assert!(
        panel.rows[0].session_hostile,
        "a session id is an OPAQUE key: if it is painted differently, it is said"
    );
    assert!(
        !panel.rows[0].session.contains('\u{202e}'),
        "and it is masked"
    );
    assert!(!panel.note.is_empty(), "and the list says what it is");

    // `u` ASKS: undoing a session reverts everything it did.
    h.dispatch(press("u")).await.expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the question");
    assert_eq!(d.title_key, "modal-undo-session-title");
    assert!(
        d.choices.iter().any(|c| c.id == "confirm" && c.destructive),
        "undoing writes: the response is marked"
    );
    settle().await;
    assert!(backend.undone.lock().expect("deshechas").is_empty());

    // And confirming sends the RAW id, not the one that is painted.
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let requested = annotated(&backend, "the undo requested", 1, |f| {
        f.undone.lock().expect("deshechas").clone()
    })
    .await;
    assert_eq!(requested, ["agente\u{202e}1".to_owned()]);
}

/// The next `Agents` change the host sends, skipping everything else.
async fn next_agents(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::AgentsView> {
    for _ in 0..20 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update with the agents panel, not a hang")
            .expect("the host is still alive");
        if let Update::Message(m) = next
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Agents { agents } = c {
                    return agents.clone();
                }
            }
        }
    }
    panic!("no Agents change in 20 updates");
}

/// An approval that arrives with the agents panel OPEN repaints the panel,
/// without waiting for a resync.
///
/// The request reorders the list with no gesture, and a renderer that is not
/// told keeps painting the previous order: the row the reader sees
/// highlighted stops being the one the host has selected, and `u` undoes
/// another session's work. `open_approval` built that repaint and then
/// dropped it — it never reached the returned updates.
#[tokio::test]
async fn an_approval_repaints_the_open_agents_panel() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("approvals") = Some(rx);
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "app.agents").await;
    let opened = next_agents(&mut sub).await.expect("the panel is open");
    assert!(opened.rows.is_empty(), "nobody has asked for anything yet");

    tx.send(norte_proto::methods::PolicyApprovalRequired {
        approval_id: 21,
        session: Some("agent-new".to_owned()),
        op: "copy".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    })
    .expect("the host is listening");

    let repainted = next_agents(&mut sub)
        .await
        .expect("the panel is still open");
    assert_eq!(
        repainted.rows.len(),
        1,
        "the new session is on screen without a resync"
    );
    assert_eq!(repainted.rows[0].last_op, "copy");
}

/// In read-only nothing is undone: it is SAID.
#[tokio::test]
async fn in_read_only_a_session_cannot_be_undone() {
    // With no approvals: a read-only window also cannot ANSWER them, so an
    // open dialog would keep the keys and this test would be checking
    // something else. The empty list holds just the same: rejection over
    // effects is checked BEFORE whether anything is selected.
    let backend = tree_as_fake();
    let backend = Arc::new(backend);
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "app.agents").await;
    let ack = h.dispatch(press("u")).await.expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-read-only"),
        "{ack:?}"
    );
    settle().await;
    assert!(backend.undone.lock().expect("deshechas").is_empty());
}

/// A request that arrives with the panel open REPAINTS it, and the
/// selection follows its session even if the list gets reordered.
///
/// The list changes with NO gesture: a new request bumps its session to
/// first place. A renderer that is not told is left painting the previous
/// order — the highlighted row stops being the one the host has chosen —
/// and `u` undoes another session's work. And the selection goes by ID, not
/// by position, which is the rule 6.2 already put in writing.
#[tokio::test]
async fn a_new_request_repaints_the_panel_and_does_not_move_the_selection() {
    let fake = tree_as_fake();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *fake.approvals.lock().expect("aprobaciones") = Some(rx);
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let request = |id: &str, aid: u64| norte_proto::methods::PolicyApprovalRequired {
        approval_id: aid,
        session: Some(id.to_owned()),
        op: "copy".to_owned(),
        paths: vec!["mem:///casa/x".to_owned()],
        paths_total: 1,
        ttl_ms: 30_000,
        detail: norte_proto::methods::ApprovalDetail::default(),
    };
    for (id, aid) in [("agente-A", 21_u64), ("agente-B", 22)] {
        tx.send(request(id, aid)).expect("the host is listening");
        let dialogs = next_dialogs(&mut sub).await;
        let d = dialogs.last().expect("the approval");
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
        let _ = next_dialogs(&mut sub).await;
    }
    run_by_palette(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let before = next_snapshot(&mut sub).await.agents.expect("open");
    assert_eq!(before.rows[0].session, "agente-B", "the most recent first");
    // The selection is set on the SECOND one, `agente-A`.
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let chosen = next_snapshot(&mut sub).await.agents.expect("open");
    assert_eq!(chosen.cursor, 1);

    // And another request for `agente-B` arrives, which was already first:
    // what changes is its count, and the list has to say it changed.
    tx.send(request("agente-B", 23))
        .expect("the host is listening");
    let mut panel = chosen.clone();
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        panel = next_snapshot(&mut sub).await.agents.expect("open");
        if panel.generation > chosen.generation {
            break;
        }
    }
    assert!(
        panel.generation > chosen.generation,
        "a list that changes on its own has to say it changed: {panel:?}"
    );
    assert_eq!(
        panel.rows[usize::try_from(panel.cursor).expect("fits")].session,
        "agente-A",
        "the selection follows ITS session, not the slot it occupied"
    );

    // The third request left its dialog up front: it is answered before
    // continuing, because the panel is modal for the mouse too.
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    if let Some(d) = snapshot.dialogs.last() {
        h.dispatch(UiAction::Dialog {
            id: d.id,
            choice: "deny".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    }

    // A click against the OLD list is refused instead of choosing for the
    // reader.
    let ack = h
        .dispatch(UiAction::AgentSelectRow {
            row: 0,
            generation: chosen.generation,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(
            &ack,
            ActionAck::Stale {
                reason: StaleAction::Generation
            }
        ),
        "{ack:?}"
    );
}

/// In read-only, the empty list does NOT say "no agent has requested
/// anything".
///
/// That window does not even subscribe to the approvals channel: its list
/// is empty for that reason, and asserting the other thing is asserting
/// what it cannot know.
#[tokio::test]
async fn in_read_only_the_pane_says_it_does_not_listen() {
    let backend = Arc::new(tree_as_fake());
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "app.agents").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let panel = next_snapshot(&mut sub).await.agents.expect("open");
    assert!(panel.rows.is_empty());
    let listening = norte_i18n::t_in(norte_i18n::Lang::Es, "agents-empty");
    assert_ne!(
        panel.empty, listening,
        "a window that is not listening cannot say nobody has requested anything"
    );
}

/// Copying the path puts BYTES on the clipboard, and it does so in both
/// modes.
///
/// Bytes and not text: a file name is bytes, and passing it through a lossy
/// decoding would paste a path that opens something else. And it mutates
/// nothing, so a read-only window also copies — it is as much a look-only
/// action as reading a name.
#[tokio::test]
async fn copying_the_path_sends_bytes_to_the_desktop() {
    for solo_read in [false, true] {
        let backend = fake_tree();
        let (h, _snap) = if solo_read {
            host_solo_read(Arc::clone(&backend)).await
        } else {
            host_tree(Arc::clone(&backend)).await
        };
        let mut native = h.native_effects();
        let mut sub = h.subscribe();
        run_by_palette(&h, &mut sub, "pane.copy-path").await;
        let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
            .await
            .expect("an effect before the deadline")
            .expect("the channel is still alive");
        match effect {
            norte_ui_host::dto::NativeEffect::CopyBytes { bytes, count } => {
                assert_eq!(count, 1);
                assert!(
                    bytes.starts_with(b"/") || bytes.starts_with(b"mem:"),
                    "the path, in its native form or the wire's: {bytes:?}"
                );
            }
            other => panic!("copying the path asks to copy, not {other:?}"),
        }
    }
}

/// In read-only NOTHING opens and no terminal is launched: it is SAID.
///
/// **A dialog's keys come from the KEYMAP, not from code** (#287).
///
/// It was the drift the shared catalogue exists to not have: the window was
/// handling its modal surfaces with fixed keys, so a preset that rebound
/// `dialog.down` changed the TUI and not the window. Here it is checked
/// against a REAL preset whose dialog keys are different.
#[tokio::test]
async fn a_dialogs_keys_are_set_by_the_preset() {
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("vim").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("vim").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("vim").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
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
    .expect("starts");
    let mut sub = h.subscribe();
    let before = selector_columns(&h, &mut sub).await;

    // The chord THIS preset binds to `dialog.down`, whatever it is.
    let bound = norte_ui_host::keys::preset_dialog_keymap("vim")
        .expect("preset")
        .bindings()
        .into_iter()
        .find(|(_, c)| *c == "dialog.down")
        .map(|(seq, _)| seq)
        .expect("the preset binds down");

    h.dispatch(UiAction::Key(chord_key(&bound)))
        .await
        .expect("host alive");
    let after = next_columns(&h, &mut sub).await;
    assert_ne!(
        after.cursor, before.cursor,
        "the preset's chord moves the cursor: {bound:?}"
    );
}

/// A painted chord, converted back into the key the host receives.
///
/// Only what is needed here: one key with its modifiers, no sequences. A
/// preset that bound `dialog.down` to two chords would fall outside this,
/// and then the test would say so instead of passing by coincidence.
pub(super) fn chord_key(chord: &str) -> norte_ui_host::keys::KeyInput {
    let parts: Vec<&str> = chord.split('+').collect();
    let (key, mods) = parts.split_last().expect("at least one part");
    let has = |m: &str| mods.iter().any(|p| p.eq_ignore_ascii_case(m));
    norte_ui_host::keys::KeyInput {
        key: (*key).to_owned(),
        ctrl: has("ctrl"),
        alt: has("alt"),
        shift: has("shift"),
        meta: has("meta") || has("cmd") || has("super"),
    }
}

/// **Editing a new one creates the EMPTY file and opens it** (#290).
///
/// The window has no editor and no terminal: what it can do is put the file
/// on disk and hand it to the desktop. And in that order — opening before
/// the outcome would launch an editor over something not there yet.
#[tokio::test]
async fn editing_a_new_one_creates_the_file_and_opens_it() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let backend = Arc::new(fake);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    run_by_palette(&h, &mut sub, "pane.edit-new").await;
    let d = next_dialogs(&mut sub).await;
    assert_eq!(d[0].title_key, "modal-new-file-title");
    assert!(d[0].input.is_some(), "a name gets typed here");
    let id = d[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "borrador.md".to_owned(),
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
    {
        let created = annotated(&backend, "the file created", 1, |f| {
            f.created.lock().expect("creados").clone()
        })
        .await;
        assert_eq!(created.len(), 1, "{created:?}");
        assert_eq!(created[0].to_wire(), "file:///casa/borrador.md");
    }

    let effect = tokio::time::timeout(WAIT_MAX, effects.recv())
        .await
        .expect("the native effect arrives")
        .expect("channel alive");
    match effect {
        norte_ui_host::dto::NativeEffect::OpenPath { path } => {
            assert_eq!(path.to_wire(), "file:///casa/borrador.md");
        }
        other => panic!("expected to open the freshly created file: {other:?}"),
    }
}

/// **And if between creating the name and opening it someone changes it, it
/// does NOT open** (#303).
///
/// norte announces the name by creating it — there is nothing to guess —
/// and whoever can write in that directory sees it appear, unlinks it and
/// leaves a symlink. The human would end up writing into a file nobody
/// showed them, and the `Created` entry's `undo` goes by PATH: undoing
/// would send to the trash whatever is there NOW.
///
/// It narrows the window and does not close it — between the `stat` and the
/// `open` there is a gap — and it is the same decision the TUI makes. The
/// file WAS CREATED: that is not undone here, it is just not opened.
#[tokio::test]
async fn a_created_item_that_stopped_being_a_file_does_not_open() {
    let mut fake = Fake {
        created_appears_as: Some(norte_proto::EntryKind::Symlink),
        ..Fake::default()
    };
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let backend = Arc::new(fake);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut effects = h.native_effects();

    run_by_palette(&h, &mut sub, "pane.edit-new").await;
    let d = next_dialogs(&mut sub).await;
    let id = d[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "borrador.md".to_owned(),
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
    {
        let created = annotated(&backend, "the file created", 1, |f| {
            f.created.lock().expect("creados").clone()
        })
        .await;
        assert_eq!(created.len(), 1, "the file WAS created: {created:?}");
    }
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(300), effects.recv())
            .await
            .is_err(),
        "the desktop is not handed what is no longer the created file"
    );
}

/// Over a REMOTE pane it is not offered, and it is said before the name gets
/// typed.
///
/// What opens afterward is the desktop application, and `xdg-open` cannot
/// be given an `sftp://`. Saying it once the name is already typed arrives
/// too late.
#[tokio::test]
async fn editing_a_new_one_is_not_offered_in_a_remote_pane() {
    let mut fake = Fake::default();
    fake.put("sftp://server/datos", vec![(b"a.txt".to_vec(), false)]);
    let backend = Arc::new(fake);
    let (h, _snap) = host_en(Arc::clone(&backend), "sftp://server/datos").await;
    let mut sub = h.subscribe();

    let ack = execute_via_palette_ack(&h, &mut sub, "pane.edit-new").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-not-local"),
        "{ack:?}"
    );
    assert!(backend.created.lock().expect("creados").is_empty());
}

/// A host that starts in a specific directory.
/// Like [`host_en`], but with a custom configuration: openers and the
/// editor are keys the window used to ignore, so the tests bring them in.
pub(super) async fn host_en_con(
    backend: Arc<Fake>,
    start: &str,
    cfg: norte_frontend::config::FrontendConfig,
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
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
    .expect("arranca")
}

pub(super) async fn host_en(
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
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
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
    .expect("arranca")
}

/// Waits for a snapshot whose first listing ends in `suffix`.
pub(super) async fn listing_in(
    sub: &mut norte_ui_host::UiSubscription,
    suffix: &str,
) -> norte_ui_host::ViewSnapshot {
    for _ in 0..30 {
        let snapshot = next_snapshot(sub).await;
        if primer_listing(&snapshot).path_display.ends_with(suffix) {
            return snapshot;
        }
    }
    panic!("the listing never reached `{suffix}`");
}

/// **Disconnecting sends the pane back to where it was BEFORE connecting**
/// (#140).
///
/// The trail backward, not "home": the pane was somewhere before jumping to
/// the machine, and that place is the answer the reader expects.
#[tokio::test]
async fn disconnecting_returns_to_where_it_was_before() {
    let mut fake = tree_as_fake();
    fake.put("sftp://server/datos", vec![(b"a.txt".to_vec(), false)]);
    *fake.connections.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: vec![norte_proto::methods::ConnectionEntry {
                name: "trabajo".to_owned(),
                url: "sftp://server/datos".to_owned(),
            }],
            unusable: Vec::new(),
        }));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Connecting for real, through the picker: it is the path a reader
    // walks, and it is what leaves the trail that gets undone afterward.
    run_by_palette(&h, &mut sub, "pane.connect").await;
    for _ in 0..20 {
        let Some(v) = next_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            break;
        }
    }
    h.dispatch(press("Enter")).await.expect("host alive");
    let _ = listing_in(&mut sub, "/datos").await;

    run_by_palette(&h, &mut sub, "pane.disconnect").await;
    let _ = listing_in(&mut sub, "/casa").await;

    let closed = backend.closed.lock().expect("cerradas");
    assert_eq!(closed.len(), 1, "{closed:?}");
    assert_eq!(closed[0].to_wire(), "sftp://server/datos");
}

/// And NEVER to another path on the same machine: that would reopen the
/// session that just closed, which is exactly what the gesture asked not to
/// have.
#[tokio::test]
async fn disconnecting_does_not_return_to_the_same_machine() {
    let mut fake = Fake::default();
    fake.put("sftp://server/uno", vec![(b"dos".to_vec(), true)]);
    fake.put("sftp://server/uno/dos", vec![(b"b.txt".to_vec(), false)]);
    let backend = Arc::new(fake);
    let (h, snap) = host_en(Arc::clone(&backend), "sftp://server/uno").await;
    let mut sub = h.subscribe();

    let b = listing(&snap);
    h.dispatch(UiAction::Activate {
        slot_id: b.slot_id,
        key: b.rows[0].key,
        generation: b.generation,
    })
    .await
    .expect("host alive");
    let _ = listing_in(&mut sub, "/uno/dos").await;

    run_by_palette(&h, &mut sub, "pane.disconnect").await;

    let mut seen = None;
    for _ in 0..30 {
        let snapshot = next_snapshot(&mut sub).await;
        let p = primer_listing(&snapshot).path_display.clone();
        if !p.contains("server") {
            seen = Some(p);
            break;
        }
    }
    let after = seen.expect("the pane leaves the closed machine");
    assert!(
        !after.contains("server"),
        "its whole trail was on that machine, so it falls back home: {after}"
    );
}

/// On a LOCAL pane there is nothing to close, and it is said.
///
/// A key that answers "done" about something that did nothing teaches
/// people not to trust the message.
#[tokio::test]
async fn in_a_local_pane_there_is_no_connection_to_close() {
    let mut fake = Fake::default();
    fake.put("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let backend = Arc::new(fake);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();

    let ack = execute_via_palette_ack(&h, &mut sub, "pane.disconnect").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "msg-disconnect-local"),
        "{ack:?}"
    );
    assert!(
        backend.closed.lock().expect("cerradas").is_empty(),
        "and nothing is asked of the daemon"
    );
}

/// Finds the TREE slot in a snapshot.
pub(super) fn tree_of(snap: &norte_ui_host::ViewSnapshot) -> &norte_ui_host::dto::TreeSlotView {
    snap.slots
        .iter()
        .find_map(|s| match s {
            SlotView::Tree(t) => Some(&**t),
            _ => None,
        })
        .expect("there is a tree slot")
}

/// Waits for a snapshot where the tree already has its branches.
pub(super) async fn tree_with_branches(
    sub: &mut norte_ui_host::UiSubscription,
    how_many: usize,
) -> norte_ui_host::ViewSnapshot {
    for _ in 0..20 {
        let snapshot = next_snapshot(sub).await;
        if snapshot
            .slots
            .iter()
            .any(|s| matches!(s, SlotView::Tree(t) if t.rows.len() >= how_many))
        {
            return snapshot;
        }
    }
    panic!("the tree never brought {how_many} branches");
}

/// **The tree lists ONE branch, and only when it opens** (`pane.tree`).
///
/// Lazy for the same reason the local listing does not carry sizes: one
/// that read itself whole on opening would take minutes on a large `$HOME`
/// and hours against a remote. Opening it requests the ROOT and nothing
/// else — `docs`'s children are not requested until someone expands `docs`.
#[tokio::test]
async fn the_tree_requests_a_branch_and_only_when_opening_it() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.tree").await;
    // The root and its DIRECTORY children: `docs` is there, `notes.txt` is
    // not.
    let snapshot = tree_with_branches(&mut sub, 2).await;
    let t = tree_of(&snapshot);
    assert_eq!(t.rows.len(), 2, "root + `docs`, no files: {:?}", t.rows);
    assert_eq!(t.rows[0].depth, 0);
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "the root carries its whole path: {:?}",
        t.rows[0]
    );
    assert_eq!(t.rows[1].label, "docs");
    assert_eq!(t.rows[1].depth, 1);
    assert_eq!(
        t.rows[1].children, None,
        "the inside has not been looked at yet, and that is NOT \"it's a leaf\""
    );
}

/// **The tree FOLLOWS the listing that navigates** (ADR 0102).
///
/// A pane anchored on opening and still afterward said where you were when
/// you opened it, and nothing more: entering a folder from the listing left
/// the tree's cursor where it was. It keeps revealing — it expands the
/// ancestors and moves the cursor — not re-anchoring: the root does not
/// change and what is open does not close.
#[tokio::test]
async fn the_tree_follows_the_listing_that_navigates() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let snapshot = tree_with_branches(&mut sub, 2).await;
    assert_eq!(tree_of(&snapshot).cursor, 0, "it starts at the root");

    // `docs` is the listing's first row (directories go first), so Enter
    // goes into it.
    run_by_palette(&h, &mut sub, "nav.enter").await;

    let mut seen = None;
    for _ in 0..20 {
        let snapshot = next_snapshot(&mut sub).await;
        if primer_listing(&snapshot)
            .path_display
            .ends_with("/casa/docs")
            && tree_of(&snapshot).cursor == 1
        {
            seen = Some(snapshot);
            break;
        }
    }
    let snapshot = seen.expect("the tree ends up pointing at the branch the listing is in");
    let t = tree_of(&snapshot);
    assert_eq!(t.rows[1].label, "docs");
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "and the root has not moved: {:?}",
        t.rows[0]
    );
}

/// **`pane.switch` cycles through LISTINGS — all of them — and skips the
/// side panels** (ADR 0102).
///
/// It is "the other pane" of any orthodox manager. It used to share a bind
/// with `layout.focus-next`, and that did two things wrong at once: with
/// the tree open, tab would stop on it, and with three listings on screen
/// there was no way to say "the one next to it" without counting the stops
/// in between.
///
/// `layout.focus-next` remains the whole screen's traversal, and that half
/// is checked right here: they are two rings, not one with two names.
#[tokio::test]
async fn tab_cycles_the_listings_and_skips_the_sides() {
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (200, 60)).await;
    let mut sub = h.subscribe();
    // The tree next to it, and a third listing: the split pane and the one
    // that is not.
    run_by_palette(&h, &mut sub, "pane.tree").await;
    // Drained before going back to the palette: the queue carries the
    // previous opening's envelopes, and the helper would read them as a
    // response to the new one.
    snapshot_until(&h, &mut sub, "the tree is on screen", |snapshot| {
        snapshot
            .palette
            .is_none()
            .then(|| {
                snapshot
                    .slots
                    .iter()
                    .any(|v| matches!(v, SlotView::Tree(_)))
            })
            .filter(|hay| *hay)
    })
    .await;
    run_by_palette(&h, &mut sub, "layout.split-v").await;
    let (tree_slot, split) =
        snapshot_until(&h, &mut sub, "three listings and a tree", |snapshot| {
            let listings = snapshot
                .slots
                .iter()
                .filter(|v| matches!(v, SlotView::Browser(_)))
                .count();
            let tree = snapshot.slots.iter().find_map(|v| match v {
                SlotView::Tree(t) => Some(t.slot_id),
                _ => None,
            })?;
            let focus = snapshot.focus?;
            (listings == 3).then_some((tree, focus))
        })
        .await;

    let mut seen = Vec::new();
    let mut anterior = split;
    for _ in 0..3 {
        h.dispatch(press("Tab")).await.expect("host alive");
        // Until focus MOVES: the previous envelope may still be in the
        // queue, and reading it as a response to this key is reading a
        // stale snapshot.
        let now = snapshot_until(&h, &mut sub, "focus moved", |snapshot| {
            snapshot.focus.filter(|f| *f != anterior)
        })
        .await;
        seen.push(now);
        anterior = now;
    }
    assert!(
        !seen.contains(&tree_slot),
        "tab does not stop on the tree: {seen:?}"
    );
    let different: std::collections::BTreeSet<u32> = seen.iter().copied().collect();
    assert_eq!(
        different.len(),
        3,
        "all THREE listings are reachable: {seen:?}"
    );
    assert_eq!(
        seen[2], split,
        "and three jumps go all the way around: {seen:?}"
    );

    // The other half: the screen's traversal DOES stop on the tree.
    let mut stops = Vec::new();
    for _ in 0..4 {
        h.dispatch(key_alt("o")).await.expect("host alive");
        let now = snapshot_until(&h, &mut sub, "focus moved", |snapshot| {
            snapshot.focus.filter(|f| *f != anterior)
        })
        .await;
        stops.push(now);
        anterior = now;
    }
    assert!(
        stops.contains(&tree_slot),
        "`layout.focus-next` traverses the whole screen: {stops:?}"
    );
}

/// Choosing a branch navigates the LISTING, and the tree's ROOT does not
/// move.
///
/// It is what makes keeping it open useful: if the tree re-anchored on
/// every navigation, entering a folder would collapse every open branch.
#[tokio::test]
async fn choosing_a_branch_navigates_the_listing_and_the_tree_does_not_move() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let snapshot = tree_with_branches(&mut sub, 2).await;
    let generacion = tree_of(&snapshot).generation;

    h.dispatch(UiAction::TreeActivateRow {
        row: 1,
        generation: generacion,
    })
    .await
    .expect("host alive");

    let mut seen = None;
    for _ in 0..20 {
        let snapshot = next_snapshot(&mut sub).await;
        if primer_listing(&snapshot)
            .path_display
            .ends_with("/casa/docs")
        {
            seen = Some(snapshot);
            break;
        }
    }
    let snapshot = seen.expect("the listing goes to the chosen branch");
    let t = tree_of(&snapshot);
    assert!(
        t.rows[0].label.ends_with("/casa"),
        "the tree stays anchored where it was: {:?}",
        t.rows[0]
    );
}

/// Regression (2026-09-21): with focus on the LEFT listing, clicking the
/// tree and choosing a branch navigates THAT listing, not the right one.
///
/// Clicking the tree gives the tree slot the active role; `active()` has to
/// answer with a listing, and it was falling back to the lowest id — which
/// in this layout, a real session's, is the one on the right.
#[tokio::test]
async fn the_tree_branch_goes_to_the_last_focused_listing() {
    let layout = r#"{"split": {"children": [{"split": {"children": [
        {"slot": {"id": 2, "kind": "browser"}}, {"slot": {"id": 1, "kind": "browser"}}],
        "dir": "horizontal", "sizes": [{"weight": 1}, {"weight": 1}]}},
        {"slot": {"id": 4, "kind": "status"}}], "dir": "vertical",
        "sizes": [{"weight": 1}, {"fixed": 1}]}}"#;
    let tree: norte_frontend::layout::Node = serde_json::from_str(layout).expect("tree");
    let (h, _) = super::base::host_with_tree(fake_tree(), tree, (160, 50)).await;
    let mut sub = h.subscribe();
    // The one on the left, 2, with focus.
    let _ = h
        .dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let snapshot = tree_with_branches(&mut sub, 2).await;
    let t = tree_of(&snapshot);
    // And the click on the tree, which focuses it, before choosing the
    // branch.
    let _ = h
        .dispatch(UiAction::FocusSlot { slot_id: t.slot_id })
        .await
        .expect("host alive");
    h.dispatch(UiAction::TreeActivateRow {
        row: 1,
        generation: t.generation,
    })
    .await
    .expect("host alive");
    let path = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Browser(b) if b.slot_id == id => Some(b.path_display.clone()),
            _ => None,
        })
    };
    let snapshot = snapshot_until(&h, &mut sub, "some listing in docs", |s| {
        (path(s, 1)?.ends_with("/casa/docs") || path(s, 2)?.ends_with("/casa/docs"))
            .then(|| s.clone())
    })
    .await;
    assert!(
        path(&snapshot, 2).is_some_and(|r| r.ends_with("/casa/docs")),
        "it navigates the one on the left, which had focus: {:?}",
        path(&snapshot, 2)
    );
    assert!(
        path(&snapshot, 1).is_some_and(|r| !r.ends_with("/casa/docs")),
        "and the one on the right is not touched"
    );
}

/// A click with ANOTHER painted generation is rejected, it does not
/// navigate.
///
/// A branch's children land IN THE MIDDLE of the list, so between the
/// reader releasing the button and the host attending to it, that row can
/// be a different folder (ADR 0068).
#[tokio::test]
async fn a_branch_from_another_painted_one_does_not_navigate() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tree").await;
    let snapshot = tree_with_branches(&mut sub, 2).await;
    let generacion = tree_of(&snapshot).generation;

    let ack = h
        .dispatch(UiAction::TreeActivateRow {
            row: 1,
            generation: generacion + 99,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "a generation that does not match is rejected: {ack:?}"
    );
    let snapshot = next_snapshot_after_resync(&h, &mut sub).await;
    assert!(
        primer_listing(&snapshot).path_display.ends_with("/casa"),
        "and the listing did not move: {:?}",
        primer_listing(&snapshot).path_display
    );
}

/// **Dropping files does NOT copy: it asks** (#283).
///
/// A drop is a gesture with no confirmation by nature, and the list is
/// composed by another process. Showing it before writing is the reader's
/// only chance to see that what arrived is not what they dragged.
#[tokio::test]
async fn dropping_asks_before_copying() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(UiAction::FilesDropped {
        paths: vec!["/tmp/uno.txt".to_owned(), "/tmp/dos.txt".to_owned()],
    })
    .await
    .expect("host alive");

    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1);
    assert_eq!(dialogs[0].title_key, "modal-drop-title");
    let body = &dialogs[0].body;
    assert_eq!(body.len(), 2, "what arrived, line by line: {body:?}");
    let dest = dialogs[0]
        .destination
        .as_ref()
        .expect("says where it lands");
    assert!(
        dest.text.ends_with("/casa"),
        "the active pane, in ITS OWN field: {dest:?}"
    );
    assert!(
        backend.transfers.lock().expect("transferencias").is_empty(),
        "opening the dialog copies nothing"
    );
}

/// Confirmed, it COPIES — it never moves — and the pane's marks are not
/// touched.
///
/// Moving what another application dragged would delete it from wherever
/// that process has it, and this window has not asked that. And the active
/// pane's marks were put there by the reader for something else: what gets
/// copied did not come from there.
#[tokio::test]
async fn a_confirmed_drop_copies_and_respects_the_marks() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listing(&snap);
    let slot = b.slot_id;
    let row = b.rows.first().expect("there are rows");
    h.dispatch(UiAction::ToggleMark {
        slot_id: slot,
        key: row.key,
        generation: b.generation,
    })
    .await
    .expect("host alive");

    h.dispatch(UiAction::FilesDropped {
        paths: vec!["/tmp/uno.txt".to_owned()],
    })
    .await
    .expect("host alive");
    let dialogs = next_dialogs(&mut sub).await;
    let id = dialogs[0].id;
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    {
        let ts = annotated(&backend, "the drop's copy", 1, |f| {
            f.transfers.lock().expect("transferencias").clone()
        })
        .await;
        assert_eq!(ts.len(), 1, "{ts:?}");
        let (from, to, mover, _) = &ts[0];
        assert!(!*mover, "a drop COPIES, it never moves: {ts:?}");
        assert_eq!(from.to_wire(), "file:///tmp/uno.txt");
        assert_eq!(
            to.to_wire(),
            "mem:///casa/uno.txt",
            "it lands in the pane's directory, with the name it carried"
        );
    }

    let snapshot = next_snapshot_after_resync(&h, &mut sub).await;
    let b = listing(&snapshot);
    assert!(
        b.rows.iter().any(|r| r.marked),
        "the reader's mark is still where it was: {:?}",
        b.rows
    );
}

/// What arrives and is not a path on this machine is SAID, not ignored.
///
/// A sender composes the `text/uri-list` by hand if it wants to. Swallowing
/// it in silence would leave the reader looking at a pane that did not
/// change with no idea why.
#[tokio::test]
async fn dropping_something_that_is_not_a_path_is_reported() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    let ack = h
        .dispatch(UiAction::FilesDropped {
            paths: vec!["relativa/mala".to_owned(), String::new()],
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, norte_ui_host::ActionAck::Unavailable { reason_key } if reason_key == "host-drop-unusable"),
        "{ack:?}"
    );
    // By `Resync` and not waiting for the next snapshot: refusing does not
    // send one, only the bar's patch, and a `next_snapshot` here would
    // hang instead of turning red.
    let snapshot = next_snapshot_after_resync(&h, &mut sub).await;
    assert!(snapshot.dialogs.is_empty(), "and no dialog opens");
    assert!(
        snapshot
            .status
            .message
            .as_deref()
            .is_some_and(|m| m == norte_i18n::t_in(norte_i18n::Lang::Es, "host-drop-unusable")),
        "and it SAYS so in the bar: {:?}",
        snapshot.status.message
    );
    assert!(backend.transfers.lock().expect("transferencias").is_empty());
}

/// **The connections picker is filled by the DAEMON** (#264): the window
/// does not read `connections.toml`, which is what would cost it pulling in
/// the whole network stack.
///
/// And the URL is masked as an AUTHORITY, not as a path: here "which
/// machine am I connecting to?" is the only question the picker answers.
#[tokio::test]
async fn the_connections_selector_is_filled_by_the_daemon() {
    let fake = tree_as_fake();
    *fake.connections.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: vec![
                norte_proto::methods::ConnectionEntry {
                    name: "trabajo".to_owned(),
                    url: "sftp://oscar@server.example/datos".to_owned(),
                },
                norte_proto::methods::ConnectionEntry {
                    name: "archivo".to_owned(),
                    url: "s3://mi-bucket".to_owned(),
                },
            ],
            unusable: Vec::new(),
        }));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.connect").await;

    let mut with_rows = None;
    for _ in 0..20 {
        let Some(v) = next_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            with_rows = Some(v);
            break;
        }
    }
    let v = with_rows.expect("the daemon's list arrives");
    assert_eq!(v.rows.len(), 2);
    assert_eq!(v.rows[0].label, "trabajo");
    assert!(
        v.rows[0].detail.contains("server.example"),
        "the detail is the URL: {:?}",
        v.rows[0]
    );
}

/// **A connection the daemon failed to read is VISIBLE, at the back, and
/// with the reason** (#365).
///
/// Before this, a single bad entry made the whole `connection.list` fail
/// and the picker opened empty: the reader lost the list of ALL their
/// connections over one, with an error that named none of them. Now the
/// good ones are listed and the bad one stays at the bottom, visible, with
/// no destination and saying what is wrong with it — which is the only
/// thing the reader can act on.
#[tokio::test]
async fn an_unreadable_connection_does_not_hide_the_others_in_the_window() {
    let fake = tree_as_fake();
    *fake.connections.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: vec![norte_proto::methods::ConnectionEntry {
                name: "buena".to_owned(),
                url: "sftp://server.example/datos".to_owned(),
            }],
            unusable: vec![norte_proto::methods::ConnectionProblem {
                name: "rota".to_owned(),
                reason: "unknown field `password`".to_owned(),
            }],
        }));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.connect").await;

    let mut with_rows = None;
    for _ in 0..20 {
        let Some(v) = next_selector(&mut sub).await else {
            continue;
        };
        if !v.rows.is_empty() {
            with_rows = Some(v);
            break;
        }
    }
    let v = with_rows.expect("the daemon's list arrives");
    assert_eq!(v.rows.len(), 2, "both are visible: {:?}", v.rows);
    assert_eq!(v.rows[0].label, "buena", "and the usable one goes first");
    assert_eq!(v.rows[1].label, "rota");
    assert!(
        v.rows[1].detail.contains("password"),
        "the unusable one says WHAT is wrong with it where its URL would go: {:?}",
        v.rows[1]
    );
}

/// With none configured, the picker SAYS so. "You have none" and "has not
/// answered yet" are not the same, and an empty list with no phrase always
/// reads as the first one.
#[tokio::test]
async fn with_no_connections_configured_the_selector_says_so() {
    let fake = tree_as_fake();
    *fake.connections.lock().expect("conexiones") =
        Some(Ok(norte_proto::methods::ConnectionListResult {
            connections: Vec::new(),
            unusable: Vec::new(),
        }));
    let backend = Arc::new(fake);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.connect").await;

    let mut seen = None;
    for _ in 0..20 {
        let Some(v) = next_selector(&mut sub).await else {
            continue;
        };
        if v.empty == norte_i18n::t_in(norte_i18n::Lang::Es, "picker-connections-empty") {
            seen = Some(v);
            break;
        }
    }
    let v = seen.expect("the empty-list phrase arrives");
    assert!(v.rows.is_empty());
}

/// **With the window up front, no desktop notice fires** (#285): the bar and
/// the board already say the same thing, and repeating it outside is noise.
#[tokio::test]
async fn with_the_window_in_front_no_outside_warning_is_shown() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut native = h.native_effects();
    let mut sub = h.subscribe();
    h.dispatch(press("F8")).await.expect("host alive");
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
    tx.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    settle().await;

    assert!(
        native.try_recv().is_err(),
        "with focus in place, no notice fires to the desktop"
    );
}

/// And with no focus it DOES, with the name of what was inside (#285).
///
/// The name is MASKED as in the listing: a notification ends up in the
/// desktop's history and can show on the lock screen, so what cannot
/// pretend here cannot pretend there either.
#[tokio::test]
async fn without_focus_the_notice_appears_and_carries_the_name() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut native = h.native_effects();
    let mut sub = h.subscribe();
    h.dispatch(UiAction::WindowFocus { focused: false })
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
    let _ = next_tasks(&mut sub).await;

    let tx = backend
        .progress
        .lock()
        .expect("progreso")
        .clone()
        .expect("there is a task");
    tx.send_modify(|p| {
        p.state = norte_proto::TaskState::Completed;
        p.current = Some(VPath::parse("mem:///casa/notas.txt").expect("vpath"));
    });

    let mut seen = None;
    for _ in 0..2_000 {
        if let Ok(norte_ui_host::dto::NativeEffect::Notify { title, body }) = native.try_recv() {
            seen = Some((title, body));
            break;
        }
        settle().await;
    }
    let (title, body) = seen.expect("with no focus, the notice fires");
    assert!(!title.is_empty(), "the notice says WHAT happened");
    assert!(body.contains("notas.txt"), "and with which file: {body:?}");
}

/// **With just ONE pane, copying asks the desktop for the destination**
/// (#284).
///
/// It used to be refused: whoever had not split the window could not copy.
/// What is checked here is the whole chain — the effect fires, the response
/// comes in, and the transfer ends up going where it was chosen — because
/// each half on its own says nothing about the other.
#[tokio::test]
async fn with_one_pane_the_desktop_chooses_the_destination() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut native = h.native_effects();
    let mut sub = h.subscribe();

    // F5 with a single listing: instead of refusing, the effect fires.
    h.dispatch(press("F5")).await.expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("the effect fires before the timeout")
        .expect("channel alive");
    let from = match effect {
        norte_ui_host::dto::NativeEffect::PickDirectory { from } => from,
        other => panic!("expected the folder picker: {other:?}"),
    };
    assert_eq!(
        from.to_wire(),
        "mem:///casa",
        "the picker opens where the pane is"
    );

    // And the response comes in through the same door as the rest.
    h.dispatch(UiAction::DirectoryPicked {
        path: Some("/tmp".to_owned()),
    })
    .await
    .expect("host alive");

    // What comes out is the usual confirmation, with THAT destination: the
    // reader sees where their files are going before a single byte moves,
    // which is what pins down that the path came from outside.
    let dialogs = next_dialogs(&mut sub).await;
    let confirmation = dialogs.last().expect("there is a confirmation");
    let dest = confirmation
        .destination
        .as_ref()
        .expect("the confirmation NAMES the destination");
    assert!(
        dest.text.contains("/tmp"),
        "the chosen destination is shown: {dest:?}"
    );
}

/// Closing the picker without choosing copies nothing: cancelling is a
/// response.
#[tokio::test]
async fn closing_the_selector_without_choosing_does_not_transfer() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut native = h.native_effects();
    h.dispatch(press("F5")).await.expect("host alive");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("the effect fires");

    h.dispatch(UiAction::DirectoryPicked { path: None })
        .await
        .expect("host alive");
    settle().await;
    assert!(
        backend.transfers.lock().expect("transferencias").is_empty(),
        "cancelling the picker transfers nothing"
    );
}

/// A picker response NOBODY requested is not interpreted. It is the same
/// rule as a stale dialog: on a surface that moves files, a stray message
/// cannot start an operation.
#[tokio::test]
async fn a_destination_nobody_asked_for_does_nothing() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(UiAction::DirectoryPicked {
            path: Some("/tmp".to_owned()),
        })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Stale { .. }),
        "with no picker open, the response is stale: {ack:?}"
    );
    settle().await;
    assert!(backend.transfers.lock().expect("transferencias").is_empty());
}

/// What an editor or a shell does with the files is not decided by this
/// window, so one mounted with no effects does not launch them.
#[tokio::test]
async fn in_read_only_nothing_from_the_desktop_is_launched() {
    let backend = fake_tree();
    let (h, _snap) = host_solo_read(Arc::clone(&backend)).await;
    let mut native = h.native_effects();
    let mut sub = h.subscribe();
    // They are not even OFFERED: the palette is built with this window's
    // effects, and offering what is going to be refused is promising
    // something that will not happen. It is the same rule that already
    // governs copying and deleting.
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let _ = next_palette(&mut sub).await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let p = next_snapshot(&mut sub).await.palette.expect("open");
    for cmd in ["pane.open", "app.terminal"] {
        assert!(
            !p.rows.iter().any(|r| r.text == cmd),
            "{cmd} is not offered in a window with no effects"
        );
    }
    // And copying the path IS, because it launches nothing.
    assert!(p.rows.iter().any(|r| r.text == "pane.copy-path") || p.total > 0);
    assert!(
        matches!(
            native.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "and no effect fired"
    );
}

/// What is not on THIS disk is not handed to the desktop.
///
/// `xdg-open` cannot be given an `sftp://`, and a terminal has nowhere to
/// sit inside one. It is refused saying so, instead of opening something
/// else — `$HOME`, typically — without warning.
#[tokio::test]
async fn a_path_that_is_not_local_does_not_open() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut native = h.native_effects();
    let mut sub = h.subscribe();
    for cmd in ["pane.open", "app.terminal"] {
        let ack = execute_via_palette_ack(&h, &mut sub, cmd).await;
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-not-local"),
            "{cmd} over a `mem://`: {ack:?}"
        );
    }
    assert!(
        matches!(
            native.try_recv(),
            Err(tokio::sync::broadcast::error::TryRecvError::Empty)
        ),
        "and no effect fired"
    );
}

/// With nobody listening to native effects, the gesture is refused: no
/// acknowledgment is given for something that is not going to happen.
#[tokio::test]
async fn without_a_desktop_behind_it_copying_is_refused() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // NOBODY calls `native_effects()`: it is the case of a frontend that
    // does not know how to do these things.
    let ack = execute_via_palette_ack(&h, &mut sub, "pane.copy-path").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-desktop"),
        "{ack:?}"
    );
}

/// A preset that rebinds `dialog.confirm` changes the window TOO (#287).
///
/// This window's dialogs were handled with fixed keys, so whoever rebound
/// the verb changed the TUI and not the window — which is exactly the drift
/// the shared catalogue exists to not have. With a field open there is
/// still a fixed regime, because there is no `dialog.*` verb for "type a
/// letter"; the test next to this one covers that.
#[tokio::test]
async fn a_rebound_key_answers_the_dialog() {
    let backend = fake_tree();
    // A user layer that binds `s` to confirm, on top of the usual preset:
    // it is what a `keymap.toml` would do.
    let layer = norte_frontend::keymap::parse_keymap(
        "[dialog]\nappend_keymap = [{ on = [\"z\"], run = \"dialog.confirm\" }]\n",
    )
    .expect("the layer parses");
    let base = norte_frontend::keymap::parse_keymap(
        norte_frontend::keymap::presets::source("orthodox").expect("preset"),
    )
    .expect("the preset parses");
    let dialog = norte_frontend::keymap::Effective::build_for(
        &base,
        std::slice::from_ref(&layer),
        norte_ui_host::commands::IMPLEMENTED_DIALOG,
        norte_frontend::keymap::Screen::Dialog,
    )
    .expect("effective");
    let (h, _snap) = UiHost::start(UiHostOptions {
        backend: Arc::clone(&backend) as Arc<dyn norte_ui_host::backend::HostBackend>,
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: dialog,
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
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
    .expect("starts");
    let mut sub = h.subscribe();

    // A delete opens its confirmation, which has NO field to type into.
    run_by_palette(&h, &mut sub, "pane.delete").await;
    let dialogs = next_dialogs(&mut sub).await;
    assert_eq!(dialogs.len(), 1, "the confirmation");
    h.dispatch(press("z")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        next_snapshot(&mut sub).await.dialogs.is_empty(),
        "`z` bound to `dialog.confirm` answers the question"
    );
    until(&backend, "the delete queued", |f| {
        (!f.deleted.lock().expect("borrados").is_empty()).then_some(())
    })
    .await;
}

/// With a FIELD open, the dialog's keys are letters.
///
/// There is no `dialog.*` verb for "type a letter", so resolving through
/// the keymap there would turn typing a file name into answering the
/// question. It is the same pair of regimes as the TUI and the 6.4
/// `[config]` editor.
#[tokio::test]
async fn with_a_field_open_keys_do_not_respond() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.mkdir").await;
    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the name prompt");
    assert!(d.input.is_some(), "this dialog has somewhere to type");
    // Any letter at all: it neither answers nor closes.
    h.dispatch(press("y")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        next_snapshot(&mut sub).await.dialogs.len(),
        1,
        "the prompt stays open"
    );
}

/// Mark all, invert, and by PATTERN (#289).
///
/// What matches is decided by the shared model (`mark_glob`), which folds
/// the name before comparing: here it is only checked that the gesture
/// arrives and that a glob that fails to compile is SAID instead of doing
/// nothing.
#[tokio::test]
async fn mark_all_invert_and_by_pattern() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let marks = |s: &norte_ui_host::ViewSnapshot| listing(s).marks;

    run_by_palette(&h, &mut sub, "mark.all").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let all = marks(&next_snapshot(&mut sub).await);
    assert!(all > 0, "marking all marks something");

    run_by_palette(&h, &mut sub, "mark.invert").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        marks(&next_snapshot(&mut sub).await),
        0,
        "inverting over everything marked leaves none"
    );

    // By pattern: the prompt asks for the glob and `Enter` applies it.
    run_by_palette(&h, &mut sub, "mark.pattern-add").await;
    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the glob prompt");
    assert!(d.input.is_some());
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "*".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        marks(&next_snapshot(&mut sub).await),
        all,
        "`*` marks the same as marking all"
    );

    // And a glob that fails to compile is refused, SAYING SO.
    run_by_palette(&h, &mut sub, "mark.pattern-remove").await;
    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the prompt");
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "[".to_owned(),
    })
    .await
    .expect("host alive");
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d.id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host alive");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "err-bad-pattern"),
        "{ack:?}"
    );
}

/// The board gets scrolled through and dismissed with the keyboard, without
/// focusing the processes panel (#292).
///
/// And a LIVE task is not dismissed: stopping it is `task.cancel`, and
/// removing from view something still writing to disk is losing sight of
/// exactly what has to be watched.
#[tokio::test]
async fn the_dashboard_is_traversed_and_discarded() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // With no tasks: all three say so instead of staying silent.
    for cmd in ["task.next", "task.prev", "task.dismiss"] {
        let ack = execute_via_palette_ack(&h, &mut sub, cmd).await;
        assert!(matches!(ack, ActionAck::Applied { .. }), "{cmd}: {ack:?}");
    }

    // A live task: dismissing it is refused.
    run_by_palette(&h, &mut sub, "pane.mkdir").await;
    let dialogs = next_dialogs(&mut sub).await;
    let d = dialogs.last().expect("the prompt");
    h.dispatch(UiAction::DialogInput {
        id: d.id,
        text: "nueva".to_owned(),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Dialog {
        id: d.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    let vivas = snapshot_until(&h, &mut sub, "the mkdir's task on the board", |f| {
        (!f.tasks.is_empty()).then(|| f.tasks.clone())
    })
    .await;
    assert!(!vivas.is_empty(), "the mkdir's task reached the board");
    if vivas
        .iter()
        .any(|t| matches!(t.state, norte_ui_host::dto::TaskStateView::Running))
    {
        let ack = execute_via_palette_ack(&h, &mut sub, "task.dismiss").await;
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key }
                if reason_key == "host-task-running"),
            "a live one is not dismissed: {ack:?}"
        );
    }

    // Once it finishes, it is: the row disappears from the board.
    snapshot_until(&h, &mut sub, "no task running", |f| {
        f.tasks
            .iter()
            .all(|t| !matches!(t.state, norte_ui_host::dto::TaskStateView::Running))
            .then_some(())
    })
    .await;
    run_by_palette(&h, &mut sub, "task.dismiss").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        next_snapshot(&mut sub).await.tasks.is_empty(),
        "the finished row is dismissed"
    );
}

/// Splitting puts another LISTING next to it, in the same directory and
/// with focus (#291).
#[tokio::test]
async fn splitting_opens_another_listing_and_gives_it_focus() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let before = snap.slots.len();
    let dir_before = listing(&snap).path_display.clone();

    run_by_palette(&h, &mut sub, "layout.split-v").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(snapshot.slots.len(), before + 1, "there is one more slot");
    let listings: Vec<&norte_ui_host::dto::BrowserSlotView> = snapshot
        .slots
        .iter()
        .filter_map(|s| match s {
            SlotView::Browser(b) => Some(&**b),
            _ => None,
        })
        .collect();
    assert!(listings.len() >= 2, "and it is a listing");
    assert!(
        listings.iter().all(|b| b.path_display == dir_before),
        "the new one starts where the one that was split was: {listings:?}"
    );
    // Focus to the newborn: splitting is asking for room to work in it.
    let focused = snapshot.focus.expect("there is focus");
    assert!(
        !snapshot.slots.is_empty() && focused != 1,
        "focus moved to the new slot: {focused}"
    );
}

/// Splitting a slot that no longer fits two is REFUSED, and it says so.
///
/// The same rule as the TUI and through the same place (ADR 0077): without
/// it the tree was left with a slot the layout hid inside the same frame —
/// the `Split` does not fit, it degrades to tabs, and the screen keeps
/// showing one.
#[tokio::test]
async fn splitting_with_no_room_is_refused_and_reported() {
    // 24 rows tall for the whole body: enough for one listing and not for
    // two (`browser`'s minimum is 5, and the chrome takes its share).
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (100, 9)).await;
    let mut sub = h.subscribe();
    let before = snap
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    let ack = execute_via_palette_ack(&h, &mut sub, "layout.split-v").await;
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-layout-split-no-room".to_owned()
        },
        "{ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    let listings = snapshot
        .slots
        .iter()
        .filter(|s| matches!(s, SlotView::Browser(_)))
        .count();
    assert_eq!(listings, before, "the tree did not keep an invisible slot");
}

/// Closing the LAST listing is refused, and it says so.
///
/// A screen with no usable listing is not a screen, it is a hang with
/// borders — the same rule the shared layout already applies on its own.
#[tokio::test]
async fn the_last_listing_does_not_close() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // `simple` has ONE listing: closing it would leave the screen with none.
    let ack = execute_via_palette_ack(&h, &mut sub, "layout.close-slot").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key }
            if reason_key == "msg-layout-last-panel"),
        "{ack:?}"
    );

    // With two, closing one does work. By KEY and not by palette: splitting
    // changes the whole screen and sends its snapshot, and the palette
    // helper reads snapshots.
    run_by_palette(&h, &mut sub, "layout.split-h").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let _ = next_snapshot(&mut sub).await;
    run_by_palette(&h, &mut sub, "layout.close-slot").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(
        snapshot
            .slots
            .iter()
            .filter(|s| matches!(s, SlotView::Browser(_)))
            .count(),
        1,
        "there is one again"
    );
}

/// The three auxiliary slots this window knows how to paint open and close
/// with their command (#291).
#[tokio::test]
async fn auxiliary_slots_open_and_close() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    for (cmd, presente) in [
        (
            "layout.processes",
            (|s: &norte_ui_host::ViewSnapshot| {
                s.slots
                    .iter()
                    .any(|v| matches!(v, SlotView::Processes { .. }))
            }) as fn(&norte_ui_host::ViewSnapshot) -> bool,
        ),
        ("layout.metadata", |s| {
            s.slots.iter().any(|v| matches!(v, SlotView::Metadata(_)))
        }),
        ("layout.places", |s| {
            s.slots.iter().any(|v| matches!(v, SlotView::Places(_)))
        }),
    ] {
        run_by_palette(&h, &mut sub, cmd).await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        assert!(
            presente(&next_snapshot(&mut sub).await),
            "{cmd} opens its slot, and this window PAINTS it (not grayed out)"
        );
        run_by_palette(&h, &mut sub, cmd).await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        assert!(
            !presente(&next_snapshot(&mut sub).await),
            "{cmd} again closes it"
        );
    }
}

/// A host with REAL configuration layers, for profiles.
///
/// Profiles live in `profiles/` of the user layer, and the host receives
/// them already resolved (ADR 0066 D14): without handing them over, there
/// is nowhere to look.
pub(super) async fn host_with_layers(
    dir_user: &std::path::Path,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    host_with_layers_and_favorites(dir_user, Vec::new()).await
}

/// A host with TWO layers: system underneath, user on top.
///
/// "Reset" needs it: removing the key from the user layer does not return
/// the factory value if the one below sets the same one, and that cannot be
/// checked with a single layer. The startup configuration is taken from the
/// theme the user wrote, which is what the window would have set.
pub(super) async fn host_with_stacked_layers(
    dir_system: &std::path::Path,
    dir_user: &std::path::Path,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_ui_host::settings::{ConfigLayer, HostPath, HostPaths};
    let mut settings = test_settings();
    settings.common.ui_theme = Some("tokyonight".to_owned());
    UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (120, 40),
        settings,
        paths: HostPaths {
            config_layers: vec![
                (
                    ConfigLayer::System,
                    HostPath {
                        path: dir_system.to_path_buf(),
                        missing: false,
                    },
                ),
                (
                    ConfigLayer::User,
                    HostPath {
                        path: dir_user.to_path_buf(),
                        missing: false,
                    },
                ),
            ],
            ..HostPaths::default()
        },
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

/// The same one, with favorites ALREADY loaded: the window reads them at
/// startup, so a test that only writes `norte.toml` sets up a host that does
/// not see them.
pub(super) async fn host_with_layers_and_favorites(
    dir_user: &std::path::Path,
    favorites: Vec<(&str, &str)>,
) -> (UiHost, norte_ui_host::ViewSnapshot) {
    use norte_ui_host::settings::{ConfigLayer, HostPath, HostPaths};
    let mut settings = test_settings();
    settings.common.hotlist = favorites
        .into_iter()
        .map(|(name, dest)| norte_config::HotlistItem {
            name: name.to_owned(),
            target: VPath::parse(dest).map_err(|_| "hotlist-invalid".to_owned()),
        })
        .collect();
    UiHost::start(UiHostOptions {
        backend: fake_tree(),
        initial_dir: dir(),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
        viewport: (120, 40),
        settings,
        paths: HostPaths {
            config_layers: vec![(
                ConfigLayer::User,
                HostPath {
                    path: dir_user.to_path_buf(),
                    missing: false,
                },
            )],
            ..HostPaths::default()
        },
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

/// The profile picker: it shows them, and choosing one APPLIES it.
///
/// A profile is for making the workspace look and behave differently, so
/// what is checked is that the change reaches the screen: here, through the
/// theme, which is what shows.
#[tokio::test]
async fn the_profiles_selector_shows_and_the_chosen_one_is_applied() {
    use norte_ui_host::dto::NativeEffect;
    let root = tempfile::tempdir().expect("temp");
    let snapshots = root.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&snapshots).expect("mkdir");
    std::fs::write(
        snapshots.join("norte.toml"),
        "[profile]\ntitle = \"Fotos\"\n\n[ui]\ntheme = \"nord\"\n",
    )
    .expect("write");

    let (h, _snap) = host_with_layers(root.path()).await;
    let mut sub = h.subscribe();
    let mut native = h.native_effects();

    run_by_palette(&h, &mut sub, "profile.pick").await;
    // The list arrives from a background task: the snapshot that carries
    // it is the one to wait for, not just the next one that goes by.
    //
    // A hundred rounds and not six: six is a deadline, not a wait. The
    // background task competes with the rest of the suite for the
    // runtime, and under load — the machine compiling alongside — it
    // used to run past and the test would turn red with nothing broken.
    // A hundred is the same order as this file's neighboring waits.
    let mut selector = None;
    for _ in 0..100 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if let Some(p) = next_snapshot(&mut sub).await.profiles {
            selector = Some(p);
            break;
        }
    }
    let p = selector.expect("the picker opened with the list");
    assert_eq!(p.rows.len(), 1, "the profile that is there: {:?}", p.rows);
    assert_eq!(p.rows[0].name, "fotos");
    assert_eq!(
        p.rows[0].title.as_deref(),
        Some("Fotos"),
        "its title comes from `[profile] title`"
    );
    assert!(!p.rows[0].active, "not set yet");

    // Choosing it applies it: its `[ui] theme` reaches the host.
    h.dispatch(press("Enter")).await.expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("the theme notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = effect else {
        panic!("the notice is the theme's: {effect:?}");
    };
    assert_eq!(name, "nord", "the PROFILE's theme, not the previous one");
}

/// And a profile's theme can be a PATH, not just a preset (ADR 0020).
///
/// The window only looked at presets, so a profile with
/// `theme = "…/mio.toml"` was left with no new colors IN SILENCE — with the
/// terminal applying it, which is the divergence. Resolving it reads a
/// file, and this window cannot read inside the actor (rule 2): it steps
/// out and comes back through the mailbox, like saving the theme does.
#[tokio::test]
async fn a_profiles_theme_can_be_a_path() {
    use norte_ui_host::dto::NativeEffect;
    let root = tempfile::tempdir().expect("temp");
    let mio = root.path().join("mio.toml");
    std::fs::write(&mio, "name = \"mio\"\n").expect("write theme");
    let snapshots = root.path().join("profiles").join("fotos");
    std::fs::create_dir_all(&snapshots).expect("mkdir");
    std::fs::write(
        snapshots.join("norte.toml"),
        format!(
            "[profile]\ntitle = \"Fotos\"\n\n[ui]\ntheme = \"{}\"\n",
            mio.display()
        ),
    )
    .expect("write");

    let (h, _snap) = host_with_layers(root.path()).await;
    let mut sub = h.subscribe();
    let mut native = h.native_effects();

    run_by_palette(&h, &mut sub, "profile.pick").await;
    let mut open = false;
    for _ in 0..100 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if next_snapshot(&mut sub).await.profiles.is_some() {
            open = true;
            break;
        }
    }
    assert!(open, "the picker opened with the list");

    h.dispatch(press("Enter")).await.expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("the theme notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = effect else {
        panic!("the notice is the theme's: {effect:?}");
    };
    assert_eq!(
        name,
        mio.display().to_string(),
        "the profile's theme was a file, and it was applied"
    );
}

/// **The window ADDS a favorite, not just opens the list** (#309).
///
/// And the name comes suggested by the SHARED model: saving REPLACES the
/// favorite already sharing that name, so with the field prefilled the
/// reflex of accepting without reading would overwrite one pointing
/// somewhere else.
#[tokio::test]
async fn the_window_saves_a_favorite_with_the_suggested_name() {
    let root = tempfile::tempdir().expect("temp");
    let (h, _snap) = host_with_layers(root.path()).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.hotlist").await;
    // `dialog.add` over the favorites list: in the terminal it is the same
    // popup's `a`.
    h.dispatch(press("a")).await.expect("host alive");
    let d = next_dialogs(&mut sub).await;
    assert_eq!(d[0].title_key, "modal-hotlist-name-title");
    assert_eq!(
        d[0].input.as_deref(),
        Some("casa"),
        "prefilled with the shared suggestion: {:?}",
        d[0].input
    );

    h.dispatch(UiAction::Dialog {
        id: d[0].id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host alive");
    // The write comes back through `spawn_blocking` and, on returning, the
    // host RE-SEEDS the favorites list that is open: waiting for the screen
    // to paint it is waiting for the file to be written, without guessing
    // how long it takes.
    // The write comes back through `spawn_blocking`, and on confirming the
    // list closes: nothing is left on screen to say "it's done". The FILE
    // is watched, which is what the test asserts, taking a turn through the
    // actor between glances instead of sleeping a fixed span.
    let written = snapshot_until(&h, &mut sub, "the favorite written to norte.toml", |_| {
        std::fs::read_to_string(root.path().join("norte.toml"))
            .ok()
            .filter(|s| s.contains("casa"))
    })
    .await;
    assert!(
        written.contains("casa"),
        "the favorite ended up in the file: {written}"
    );
}

/// And it REMOVES one, which was the other half that was missing (#309).
#[tokio::test]
async fn the_window_removes_the_favorite_under_the_cursor() {
    let root = tempfile::tempdir().expect("temp");
    std::fs::write(
        root.path().join("norte.toml"),
        "[[hotlist]]\nname = \"casa\"\npath = \"mem:///casa\"\n",
    )
    .expect("escribe");
    let (h, _snap) =
        host_with_layers_and_favorites(root.path(), vec![("casa", "mem:///casa")]).await;
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "pane.hotlist").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    let rows = snapshot.picker.as_ref().map_or(0, |p| p.rows.len());
    assert_eq!(
        rows, 1,
        "the list carries the favorite: {:?}",
        snapshot.picker
    );
    // `dialog.remove`: the terminal popup's `d`.
    let ack = h.dispatch(press("d")).await.expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Applied { .. }),
        "the picker handles the key: {ack:?}"
    );
    // Same as when adding: the open list re-seeds when the write comes back,
    // so the row leaving is the signal that the file is already updated.
    let after = snapshot_until(&h, &mut sub, "the list without the favorite", |f| {
        f.picker
            .as_ref()
            .is_some_and(|p| p.rows.is_empty())
            .then(|| f.clone())
    })
    .await;

    let written = std::fs::read_to_string(root.path().join("norte.toml")).expect("norte.toml");
    assert!(
        !written.contains("casa"),
        "the favorite left the file: {written}; filas={:?} msg={:?}",
        after.picker.as_ref().map(|p| p.rows.len()),
        after.status.message
    );
}

/// A profile that does not exist changes nothing, and it says so.
///
/// "You stay on the one you were on" is what ADR 0079 D7 asks for a change:
/// starting with no profile is recoverable, stopping halfway is not.
#[tokio::test]
async fn a_profile_that_fails_to_load_leaves_everything_as_it_was() {
    let root = tempfile::tempdir().expect("temp");
    std::fs::create_dir_all(root.path().join("profiles")).expect("mkdir");
    let (h, _snap) = host_with_layers(root.path()).await;
    let mut sub = h.subscribe();

    // With no profiles, cycling has nowhere to go — and it says so instead
    // of pretending.
    run_by_palette(&h, &mut sub, "profile.next").await;
    // With no round count: reading `profiles/` is a background task, so
    // the notice does not arrive in the next snapshot but when that task
    // answers. With six resyncs in a row, a loaded machine used to spend
    // them all before the background thread woke up, and the test turned
    // red with nothing broken — which is how a red gets learned to ignore.
    snapshot_until(
        &h,
        &mut sub,
        "the notice that there is no other profile",
        |f| f.status.message.is_some().then_some(()),
    )
    .await;
}

/// With `[ui] parent_entry`, the listing carries its `..` row — and it is
/// not an operand.
///
/// The row anyone coming from any manager in the family expects: the cursor
/// lands on it and Enter goes up. What makes it safe is that nothing is
/// ever marked on it, so a copy or a delete have nothing to act on instead
/// of acting on the parent directory.
#[tokio::test]
async fn with_the_parent_row_the_listing_puts_it_first() {
    let mut cfg = norte_ui_host::default_settings();
    cfg.common.ui_parent_entry = Some(true);
    let (_h, snap) = UiHost::start(UiHostOptions {
        backend: fake_tree(),
        // A SUBdirectory: at a root there is nowhere to go up to and the
        // row does not appear no matter how much the config turns it on.
        initial_dir: norte_proto::VPath::parse("mem:///casa").expect("wire"),
        initial_dir_requested: false,
        attach: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::preset_dialog_keymap("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
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

    let rows = snap
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Browser(b) => Some(b.rows.clone()),
            _ => None,
        })
        .expect("there is a listing");
    assert_eq!(
        rows.first().map(|r| r.display_name.as_str()),
        Some(".."),
        "the first row is the go-up one, painted `..` and not with the \
         parent's name: {rows:?}"
    );
    assert_eq!(
        rows[0].kind,
        norte_ui_host::dto::RowKind::Dir,
        "and it is a directory: Enter goes up through the same path as any other"
    );
}

/// Dragging the border splits the pair, and what one gains the other loses.
///
/// The renderer sends where the POINTER is, in cells. Which pair splits and
/// how much each one gets is decided by the host, which is the one holding
/// the layout and each kind's minimums.
#[tokio::test]
async fn dragging_the_edge_splits_the_two_slots() {
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let width = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .map(|p| p.width)
            .expect("the slot is placed")
    };
    let left = snap.layout.placements[0].slot_id;
    let right = snap.layout.placements[1].slot_id;
    let (a0, b0) = (width(&snap, left), width(&snap, right));
    assert_eq!(a0 + b0, 120, "both split the screen");

    let mut sub = h.subscribe();
    // The pointer at a third of the width.
    h.dispatch(UiAction::ResizeSlot {
        slot_id: left,
        cells: 40,
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let after = next_snapshot(&mut sub).await;
    // With ONE cell of margin: the pair renormalizes to weights between 1
    // and 100 and the layout splits again in integers, so a third of 120
    // lands on 39 or 40 depending on which way the rounding falls. Demanding
    // the exact cell would be demanding the drag not go through weights.
    let width_left = width(&after, left);
    assert!(
        width_left.abs_diff(40) <= 1,
        "the border goes where the pointer says: {width_left}"
    );
    assert_eq!(
        width(&after, left) + width(&after, right),
        a0 + b0,
        "the pair occupies the same: dragging one border does not touch the rest"
    );
}

/// The theme screen CHOOSES, and what is chosen shows.
///
/// Before, it only showed: whoever hosts this window resolves the theme
/// once on startup, so a chosen theme had no way to reach the screen. With
/// `NativeEffect::ThemeChanged` it does, and this picker is the terminal's —
/// presets, cursor on the one that is set, and LIVE preview.
#[tokio::test]
async fn the_theme_selector_chooses_and_notifies_the_host() {
    use norte_ui_host::dto::NativeEffect;
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let mut native = h.native_effects();
    let mut sub = h.subscribe();

    run_by_palette(&h, &mut sub, "app.theme").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let opened = next_snapshot(&mut sub).await;
    let theme = opened.theme.expect("the theme screen is open");
    assert!(
        theme.choices.len() > 1,
        "there is something to choose from: {:?}",
        theme.choices
    );

    // Moving down previews: the effect fires BEFORE confirming anything,
    // which is what makes the reader see the theme instead of reading its
    // name.
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    let effect = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("the theme notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = effect else {
        panic!("the notice is the theme's: {effect:?}");
    };
    assert_eq!(
        name, theme.choices[1],
        "the one that ended up under the cursor, not another"
    );

    // And `Escape` GOES BACK to the one that was set: a picker with live
    // preview that closes leaving the last one the cursor brushed is a way
    // of changing the theme by accident.
    h.dispatch(press("Escape")).await.expect("host alive");
    let return_ = tokio::time::timeout(std::time::Duration::from_secs(2), native.recv())
        .await
        .expect("the return notice fires")
        .expect("channel alive");
    let NativeEffect::ThemeChanged { name } = return_ else {
        panic!("the notice is the theme's: {return_:?}");
    };
    assert_eq!(name, theme.name, "it goes back to the one that was set");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert!(
        next_snapshot(&mut sub).await.theme.is_none(),
        "and the screen closes"
    );
}

/// The menu bar: it drops down, gets navigated, and what is chosen RUNS.
///
/// The menus and their entries are `norte_frontend::menu`, the same model
/// the TUI paints, so what is checked here is not WHAT is inside — that is
/// covered by that crate's tests — but that the window projects it,
/// navigates it and runs it through the same path as a key.
#[tokio::test]
async fn the_menu_is_traversed_and_the_chosen_one_runs() {
    let (h, snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    assert!(snap.menu.bar, "the bar is painted by default");
    assert_eq!(snap.menu.open, None, "and it is born closed");
    assert_eq!(
        snap.menu.titles.len(),
        norte_frontend::menu::MENUS.len(),
        "all the shared model's menus"
    );

    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "app.menu").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let open = next_snapshot(&mut sub).await;
    assert_eq!(open.menu.open, Some(0), "it drops down at the first one");
    assert!(
        !open.menu.items.is_empty(),
        "and it carries its entries: {:?}",
        open.menu.items
    );

    // A down arrow moves the cursor INSIDE the menu, not the listing.
    let listing_cursor = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Browser(b) => Some(b.cursor),
            _ => None,
        })
    };
    let cursor_before = listing_cursor(&open);
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let moved = next_snapshot(&mut sub).await;
    assert_eq!(moved.menu.cursor, 1);
    assert_eq!(
        listing_cursor(&moved),
        cursor_before,
        "the listing underneath did not move"
    );

    // And `Escape` closes without running anything.
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(next_snapshot(&mut sub).await.menu.open, None);
}

/// #324: the panel bar crosses the bridge with what the TUI paints — which
/// panels there are, in what order, which is open and which has the
/// keyboard — and pressing a button opens the panel through the SAME
/// dispatch as its shortcut. The new bar travels as a PATCH in the same
/// send that opens the panel, with `toggle_slot` never knowing it
/// exists.
#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario end to end; one line over since the English names made rustfmt wrap"
)]
async fn the_pane_bar_shows_the_panes_and_a_click_opens_them() {
    use norte_ui_host::dto::{PanelButtonState, ViewChange};
    let (h, snap) = host_tree(fake_tree()).await;
    let bar = &snap.panel_bar;
    assert!(bar.bar, "the bar is painted by default, like in the TUI");
    let kinds: Vec<&str> = bar.buttons.iter().map(|b| b.kind.as_str()).collect();
    assert_eq!(
        kinds,
        [
            "places",
            "viewer",
            "processes",
            "metadata",
            "tree",
            "log",
            // Phase 4: the disk map. The window does not PAINT it yet (T5),
            // but the kind belongs to the SHARED registry, so its button
            // shows up here from the moment it is declared — which is
            // exactly what this test checks: the same buttons and the same
            // order as the TUI.
            "disk-map",
            // Phase 7: the timeline, likewise — the kind belongs to the
            // shared registry, so its button shows up here from the moment
            // it is declared even though the window does not paint it yet
            // (#359).
            "timeline",
            // #362: the terminal panel, same as its two neighbors — the
            // kind belongs to the shared registry, so its button shows up
            // here from the moment it is declared even though the window
            // does not paint it yet. And in the window the kind will
            // really be needed: the grid is built by `norte-term` once for
            // both frontends (T4).
            "terminal",
        ],
        "the same buttons and the same order as `panelbar::buttons`"
    );
    let places = kinds.iter().position(|k| *k == "places").expect("places");
    let button = &bar.buttons[places];
    assert_eq!(
        button.label, "Sitios",
        "translated into the session's language"
    );
    assert_eq!(button.letter, "S");
    assert_eq!(button.state, PanelButtonState::Closed, "{bar:?}");
    assert!(
        bar.buttons.iter().all(|b| !b.attention),
        "with no tasks or notices nothing has anything new: {bar:?}"
    );

    let mut sub = h.subscribe();
    h.dispatch(UiAction::PanelBarActivate {
        button: u32::try_from(places).expect("six buttons fit in a u32"),
    })
    .await
    .expect("host alive");
    // What opens the panel CARRIES the new bar: opening a slot changes the
    // layout and goes as a SNAPSHOT, and the snapshot carries the bar; a
    // change that went as a patch would carry it as `ViewChange::PanelBar`.
    // Both forms are accepted, and with a deadline: a host that did not
    // send it would leave this `recv` waiting forever, and a hung test is
    // not a red test.
    let mut bar_new = None;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    while bar_new.is_none() {
        let next = tokio::time::timeout_at(deadline, sub.recv())
            .await
            .expect("the new bar arrives before five seconds")
            .expect("host alive");
        let Update::Message(m) = next else {
            continue;
        };
        match m.payload {
            UiUpdate::Snapshot(s) => bar_new = Some(s.panel_bar),
            UiUpdate::Patch(p) => {
                if let Some(ViewChange::PanelBar { panel_bar }) = p
                    .changes
                    .into_iter()
                    .find(|c| matches!(c, ViewChange::PanelBar { .. }))
                {
                    bar_new = Some(panel_bar);
                }
            }
            UiUpdate::Notice(_) => {}
        }
    }
    let bar = bar_new.expect("the bar travelled");
    assert_ne!(
        bar.buttons[places].state,
        PanelButtonState::Closed,
        "the places panel is open: {bar:?}"
    );

    // Opening the places bar triggers a volumes read that lands as ANOTHER
    // snapshot, later: it waits for the snapshot that shows the requested
    // state, not the next one that happens to be in the queue.
    let with_places =
        |s: &norte_ui_host::ViewSnapshot| s.slots.iter().any(|v| matches!(v, SlotView::Places(_)));
    let open = snapshot_until(&h, &mut sub, "the places slot placed", |s| {
        with_places(s).then(|| s.clone())
    })
    .await;
    assert_ne!(
        open.panel_bar.buttons[places].state,
        PanelButtonState::Closed
    );

    // The same button again CLOSES it: it is a toggle, like its shortcut.
    let ack = h
        .dispatch(UiAction::PanelBarActivate {
            button: u32::try_from(places).expect("six buttons fit in a u32"),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let closed = snapshot_until(&h, &mut sub, "the places slot closed", |s| {
        (!with_places(s)).then(|| s.clone())
    })
    .await;
    assert_eq!(
        closed.panel_bar.buttons[places].state,
        PanelButtonState::Closed
    );

    // An index the bar does not have is a stale bar: it should ask for a
    // snapshot.
    let ack = h
        .dispatch(UiAction::PanelBarActivate { button: 99 })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Stale { .. }),
        "was {ack:?}"
    );
}

/// ADR 0134 (phase F): two panels on the same border share a spot as tabs;
/// the group is a PANELS one and its tabs carry the panel's name. Pressing
/// the hidden one brings it to the front, and pressing the visible one
/// closes it and dissolves the group.
#[tokio::test]
async fn panes_on_one_edge_share_room_as_tabs() {
    let (h, _snap) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "layout.timeline").await;
    run_by_palette(&h, &mut sub, "layout.metadata").await;
    let snapshot = snapshot_until(&h, &mut sub, "a panels group", |s| {
        s.layout.tabs.iter().find(|g| g.panels).cloned()
    })
    .await;
    let titles: Vec<&str> = snapshot.tabs.iter().map(|t| t.title.as_str()).collect();
    // The panel bar's names, not the kind ids.
    assert_eq!(titles, ["Historial", "Detalles"], "{snapshot:?}");
    assert_eq!(snapshot.active, 1, "the one that arrives stays in front");

    // The timeline is hidden: pressing it SHOWS it.
    run_by_palette(&h, &mut sub, "layout.timeline").await;
    let g = snapshot_until(&h, &mut sub, "the timeline in front", |s| {
        s.layout
            .tabs
            .iter()
            .find(|g| g.panels && g.active == 0)
            .cloned()
    })
    .await;
    assert_eq!(g.tabs.len(), 2, "nothing closed");

    // Visible: now it does close it, and the group of one dissolves.
    run_by_palette(&h, &mut sub, "layout.timeline").await;
    let () = snapshot_until(&h, &mut sub, "the group dissolved", |s| {
        s.layout.tabs.iter().all(|g| !g.panels).then_some(())
    })
    .await;
}

/// ADR 0133: the snapshot carries the four layout buttons with their name,
/// pressing "split" places one more listing, and an id or a tab that do not
/// exist are a race that asks for a snapshot.
#[tokio::test]
async fn layout_and_tab_buttons_are_clickable() {
    let (h, snap) = host_tree(fake_tree()).await;
    let ids: Vec<&str> = snap.layout_buttons.iter().map(|b| b.id.as_str()).collect();
    assert_eq!(ids, ["split-h", "split-v", "equalize", "flip", "pick"]);
    assert_eq!(snap.layout_buttons[0].label, "Partir lado a lado");
    let listings = |s: &norte_ui_host::ViewSnapshot| {
        s.slots
            .iter()
            .filter(|v| matches!(v, SlotView::Browser(_)))
            .count()
    };
    let before = listings(&snap);

    let mut sub = h.subscribe();
    let ack = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let () = snapshot_until(&h, &mut sub, "one more listing", |s| {
        (listings(s) > before).then_some(())
    })
    .await;

    for action in [
        UiAction::LayoutButtonActivate {
            id: "no-existe".to_owned(),
        },
        UiAction::TabAction {
            slot_id: 9999,
            verb: norte_ui_host::TabVerb::New,
        },
    ] {
        let ack = h.dispatch(action).await.expect("host alive");
        assert!(matches!(ack, ActionAck::Stale { .. }), "was {ack:?}");
    }
}

/// Regression (2026-09-21 capture): with two listings and details on the
/// right, dragging the border between the SECOND listing and details
/// narrows them. Only that listing was being measured, not the whole body,
/// and the border was not following the pointer.
#[tokio::test]
async fn the_details_edge_is_dragged_from_the_second_listing() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let _ = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    run_by_palette(&h, &mut sub, "layout.metadata").await;
    let detalles = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Metadata(m) => Some(m.slot_id),
            _ => None,
        })
    };
    let snap = snapshot_until(&h, &mut sub, "two listings and the details", |s| {
        let n = s
            .slots
            .iter()
            .filter(|v| matches!(v, SlotView::Browser(_)))
            .count();
        (n == 2 && detalles(s).is_some()).then(|| s.clone())
    })
    .await;
    let meta_id = detalles(&snap).expect("details");
    let place = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        *s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .expect("placed")
    };
    let meta = place(&snap, meta_id);
    // The listing touching the details on the left.
    let listing = snap
        .layout
        .placements
        .iter()
        .find(|p| p.x + p.width == meta.x)
        .expect("neighbor")
        .slot_id;
    // The capture's state: the details MOVED to the right of the listing
    // come in with their weight, and three end up weighted in a layout.
    let _ = h
        .dispatch(UiAction::MoveSlot {
            slot_id: meta_id,
            target: listing,
            zone: norte_frontend::layout::DropZone::Right,
        })
        .await
        .expect("host alive");
    let snap = snapshot_until(&h, &mut sub, "details with weight", |s| {
        let m = place(s, meta_id);
        (m.width != meta.width).then(|| s.clone())
    })
    .await;
    let meta = place(&snap, meta_id);
    let other_width = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id != listing && p.slot_id != meta_id && p.y == meta.y)
        .map(|p| p.width)
        .expect("the other listing");
    let ack = h
        .dispatch(UiAction::ResizeSlot {
            slot_id: listing,
            cells: meta.x + 10,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let width = meta.width;
    let after = snapshot_until(&h, &mut sub, "details about ten cells narrower", |s| {
        let p = s.layout.placements.iter().find(|p| p.slot_id == meta_id)?;
        (p.width + 9 <= width && p.width + 11 >= width).then(|| s.clone())
    })
    .await;
    // And the other listing, which nobody grabbed, does not notice.
    let other = after
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id != listing && p.slot_id != meta_id && p.y == meta.y)
        .map(|p| p.width)
        .expect("the other listing");
    assert!(
        other.abs_diff(other_width) <= 1,
        "the third one moved: {other_width} → {other}"
    );
}

/// Dragged sizes ARE REMEMBERED: what one window leaves in the session is
/// what the next one opens with, until a different layout is chosen.
#[tokio::test]
async fn dragged_sizes_come_back_on_open() {
    let fake = super::tree_as_fake();
    // Owner of an empty session: the only one that writes.
    *fake.session.lock().expect("session") = (
        norte_proto::methods::Session {
            version: 0,
            revision: 0,
            body: serde_json::Value::Null,
        },
        true,
    );
    let fake = Arc::new(fake);
    let (h, _) = host_tree(Arc::clone(&fake)).await;
    let mut sub = h.subscribe();
    let _ = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    let snap = snapshot_until(&h, &mut sub, "two listings", |s| {
        let n = s
            .slots
            .iter()
            .filter(|v| matches!(v, SlotView::Browser(_)))
            .count();
        (n == 2).then(|| s.clone())
    })
    .await;
    let left = *snap
        .layout
        .placements
        .iter()
        .filter(|p| {
            snap.slots
                .iter()
                .any(|v| matches!(v, SlotView::Browser(b) if b.slot_id == p.slot_id))
        })
        .min_by_key(|p| p.x)
        .expect("listing");
    // The border at a third.
    let _ = h
        .dispatch(UiAction::ResizeSlot {
            slot_id: left.slot_id,
            cells: (left.x + left.width * 2) / 3,
        })
        .await
        .expect("host alive");
    let moved = snapshot_until(&h, &mut sub, "border moved", |s| {
        let p = s
            .layout
            .placements
            .iter()
            .find(|p| p.slot_id == left.slot_id)?;
        (p.width < left.width).then(|| s.clone())
    })
    .await;
    let widths = |s: &norte_ui_host::ViewSnapshot| -> Vec<(u32, u16)> {
        let mut v: Vec<(u32, u16)> = s
            .layout
            .placements
            .iter()
            .map(|p| (p.slot_id, p.width))
            .collect();
        v.sort_unstable();
        v
    };
    // What was written, as soon as it is written.
    let mut written = None;
    for _ in 0..50 {
        written = fake.written.lock().expect("escrito").clone();
        let carries = written
            .as_ref()
            .is_some_and(|b| b.to_string().contains(&format!("\"id\":{}", left.slot_id)));
        if carries {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let written = written.expect("the window wrote its session");
    drop(h);

    // Another window, over what the first one left.
    let segundo = super::tree_as_fake();
    *segundo.session.lock().expect("session") = (
        norte_proto::methods::Session {
            version: norte_frontend::session::SCHEMA_VERSION,
            revision: 9,
            body: written,
        },
        true,
    );
    let (_h2, snap2) = host_tree(Arc::new(segundo)).await;
    assert_eq!(
        widths(&snap2),
        widths(&moved),
        "it opens with the same widths"
    );
}

/// Regression (second capture): the HORIZONTAL border between the details
/// and the log below moves the log up and down, grabbed from the details.
#[tokio::test]
async fn the_log_edge_is_dragged_from_the_details() {
    // The session tree from the capture, exactly as the window saved it.
    let session = r#"{"split": {"children": [{"split": {"children": [
        {"slot": {"id": 2, "kind": "browser"}}, {"slot": {"id": 1, "kind": "browser"}},
        {"slot": {"bindings": {"follows": {"role": "active"}}, "id": 5, "kind": "metadata"}}],
        "dir": "horizontal", "sizes": [{"weight": 1}, {"weight": 1}, {"weight": 1}]}},
        {"slot": {"id": 6, "kind": "log"}}, {"slot": {"id": 3, "kind": "tasks"}},
        {"slot": {"id": 4, "kind": "status"}}], "dir": "vertical",
        "sizes": [{"weight": 1}, {"fixed": 11}, "auto", {"fixed": 1}]}}"#;
    let tree: norte_frontend::layout::Node = serde_json::from_str(session).expect("tree");
    let (h, _) = super::base::host_with_tree(fake_tree(), tree, (160, 50)).await;
    let mut sub = h.subscribe();
    let _ = h.dispatch(UiAction::Resync).await;
    let kind_de = |s: &norte_ui_host::ViewSnapshot, wants: &str| {
        s.slots.iter().find_map(|v| match (v, wants) {
            (SlotView::Metadata(m), "metadata") => Some(m.slot_id),
            (SlotView::Log(l), "log") => Some(l.slot_id),
            _ => None,
        })
    };
    let snap = snapshot_until(&h, &mut sub, "detalles y registro", |s| {
        (kind_de(s, "metadata").is_some() && kind_de(s, "log").is_some()).then(|| s.clone())
    })
    .await;
    let place = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        *s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .expect("colocado")
    };
    let meta_id = kind_de(&snap, "metadata").expect("details");
    let log_id = kind_de(&snap, "log").expect("log");
    let meta = place(&snap, meta_id);
    let log = place(&snap, log_id);
    assert_eq!(meta.y + meta.height, log.y, "the details touch the log");
    // Move the border up five rows: the log grows by five.
    let ack = h
        .dispatch(UiAction::ResizeSlot {
            slot_id: meta_id,
            cells: log.y - 5,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let alto = log.height;
    let () = snapshot_until(&h, &mut sub, "log five rows taller", |s| {
        (place(s, log_id).height == alto + 5).then_some(())
    })
    .await;
}

/// ADR 0138: dropping one listing below the other stacks them;
/// `layout.flip` puts them back side by side.
#[tokio::test]
async fn move_and_rotate_redistribute_the_listings() {
    let (h, _) = host_tree(fake_tree()).await;
    let mut sub = h.subscribe();
    let listings = |s: &norte_ui_host::ViewSnapshot| -> Vec<u32> {
        s.slots
            .iter()
            .filter_map(|v| match v {
                SlotView::Browser(b) => Some(b.slot_id),
                _ => None,
            })
            .collect()
    };
    let _ = h
        .dispatch(UiAction::LayoutButtonActivate {
            id: "split-h".to_owned(),
        })
        .await
        .expect("host alive");
    let snap = snapshot_until(&h, &mut sub, "two listings", |s| {
        (listings(s).len() == 2).then(|| s.clone())
    })
    .await;
    let [a, b] = listings(&snap)[..] else {
        unreachable!("two, from the wait")
    };
    let place = |s: &norte_ui_host::ViewSnapshot, id: u32| {
        s.layout
            .placements
            .iter()
            .find(|p| p.slot_id == id)
            .map(|p| (p.x, p.y))
    };
    let (ax, ay) = place(&snap, a).expect("a placed");
    let (bx, by) = place(&snap, b).expect("b placed");
    assert!(ay == by && ax < bx, "side by side at the start");

    let ack = h
        .dispatch(UiAction::MoveSlot {
            slot_id: a,
            target: b,
            zone: norte_frontend::layout::DropZone::Bottom,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "was {ack:?}");
    let () = snapshot_until(&h, &mut sub, "a below b", |s| {
        let (ax, ay) = place(s, a)?;
        let (bx, by) = place(s, b)?;
        (ax == bx && ay > by).then_some(())
    })
    .await;

    run_by_palette(&h, &mut sub, "layout.flip").await;
    let () = snapshot_until(&h, &mut sub, "side by side again", |s| {
        let (ax, ay) = place(s, a)?;
        let (bx, by) = place(s, b)?;
        (ay == by && ax != bx).then_some(())
    })
    .await;
}

/// ADR 0132: the snapshot carries the status bar's right half, with the
/// default items that have something to say, and pressing one runs its
/// command; an id that is no longer there is a race and asks for a
/// snapshot.
#[tokio::test]
async fn the_status_bar_brings_its_items_and_they_are_clickable() {
    let (h, snap) = host_tree(fake_tree()).await;
    let ids: Vec<&str> = snap.status_items.iter().map(|i| i.id.as_str()).collect();
    // With no marks, no tasks and no notices, those three stay silent.
    assert_eq!(
        ids,
        ["position", "sort", "encoding"],
        "{:?}",
        snap.status_items
    );
    let orden = snap
        .status_items
        .iter()
        .find(|i| i.id == "sort")
        .expect("sort");
    assert!(orden.clickable);
    assert!(!snap.status_items[0].clickable, "position is not clickable");

    let ack = h
        .dispatch(UiAction::StatusItemActivate {
            id: "sort".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(
        !matches!(ack, ActionAck::Stale { .. }),
        "sort is clickable: {ack:?}"
    );
    // `tasks` is not there (there are no tasks): what the renderer clicked
    // no longer exists.
    let ack = h
        .dispatch(UiAction::StatusItemActivate {
            id: "tasks".to_owned(),
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Stale { .. }), "was {ack:?}");
}

/// #291: the preview slot FOLLOWS the cursor and shows the same viewer as
/// the big one — with the plugin's preview and its fragments — over a
/// directory it says so, and closing it removes it. The last of ADR 0058's
/// seven kinds the window was not painting.
#[tokio::test]
async fn the_preview_slot_follows_the_cursor_and_shows_the_viewer() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"main.rs".to_vec(), false)],
    );
    f.content
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    f.previews.insert(
        "mem:///casa/main.rs".to_owned(),
        norte_proto::methods::PluginPreviewStyled {
            plugin_id: "acme.syntax".to_owned(),
            plugin_name: "Syntax".to_owned(),
            lines: vec![vec![norte_proto::methods::SpanWire {
                text: "fn main() {}".to_owned(),
                role: Some("title".to_owned()),
                fg: None,
                bg: None,
            }]],
            lossy: false,
        },
    );
    let f = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&f)).await;
    let mut sub = h.subscribe();

    // Opening it is a catalogue command, the same one as in the TUI.
    run_by_palette(&h, &mut sub, "layout.preview").await;
    let preview_de = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Preview(p) => Some(p.as_ref().clone()),
            _ => None,
        })
    };
    // The cursor is born on `..` or on `docs`: first the note. Until the
    // listing lands there is no cursor, and THAT note is a different one
    // ("nothing selected"): it waits for the directory's.
    let with_note = snapshot_until(&h, &mut sub, "the preview slot over a directory", |s| {
        preview_de(s).filter(|p| p.viewer.is_none() && p.note == "directorio")
    })
    .await;
    assert!(with_note.viewer.is_none(), "{with_note:?}");

    // Move down to the file: the slot reads it on its own, and what it
    // shows is the plugin's preview, with its fragment and its "via".
    for _ in 0..3 {
        h.dispatch(press("ArrowDown")).await.expect("host alive");
    }
    let con_visor = snapshot_until(&h, &mut sub, "the preview slot with the file", |s| {
        preview_de(s).filter(|p| p.viewer.is_some())
    })
    .await;
    let visor = con_visor.viewer.expect("viewer");
    assert!(
        visor.path_display.ends_with("main.rs"),
        "{}",
        visor.path_display
    );
    assert_eq!(visor.styled.len(), 1);
    assert_eq!(visor.styled[0][0].role.as_deref(), Some("title"));
    assert!(visor.preview_by.contains("Syntax"));
    assert!(con_visor.note.is_empty());
    // And the width requested is the SLOT's, not the window's.
    let widths = f.preview_widths.lock().expect("mutex").clone();
    assert!(
        widths.iter().all(|a| a.is_some_and(|a| a < 120)),
        "the previewer receives the slot's width: {widths:?}"
    );

    // The same command closes it, and what it was showing goes with it.
    run_by_palette(&h, &mut sub, "layout.preview").await;
    let closed = snapshot_until(&h, &mut sub, "no preview slot", |s| {
        preview_de(s).is_none().then(|| s.clone())
    })
    .await;
    assert!(closed.viewer.is_none(), "the BIG viewer did not open");
}

/// #291, second half: with FOCUS on the docked slot, the viewer's keys move
/// that viewer; the wheel moves it through the host; `viewer.close` returns
/// focus to the listing without closing the slot (like the TUI); and with
/// no focus, the arrows keep moving the listing.
#[tokio::test]
async fn the_preview_slot_with_focus_moves_with_the_viewer_keys() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"largo.txt".to_vec(), false)]);
    let text = (1..=80)
        .map(|i| format!("line {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    f.content
        .insert("mem:///casa/largo.txt".to_owned(), text.into_bytes());
    let f = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&f)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "layout.preview").await;
    let preview_de = |s: &norte_ui_host::ViewSnapshot| {
        s.slots.iter().find_map(|v| match v {
            SlotView::Preview(p) => Some(p.as_ref().clone()),
            _ => None,
        })
    };
    // Move down to the file.
    for _ in 0..3 {
        h.dispatch(press("ArrowDown")).await.expect("host alive");
    }
    let con_visor = snapshot_until(&h, &mut sub, "the preview slot with the file", |s| {
        preview_de(s).filter(|p| p.viewer.is_some())
    })
    .await;
    let v = con_visor.viewer.as_ref().expect("viewer");
    assert_eq!(v.first_line, 0);
    assert!(
        v.lines.len() < 80 && v.lines.len() <= 38,
        "the WINDOW that fits the slot travels, not the file: {}",
        v.lines.len()
    );
    let slot = con_visor.slot_id;

    // With no focus, an arrow goes to the LISTING, not the viewer. Down and
    // not up: up would change the file under the cursor, and with it what
    // the slot shows — what is measured here is who the key went to.
    h.dispatch(press("ArrowDown")).await.expect("host alive");
    let unfocused = snapshot_until(&h, &mut sub, "the arrow went to the listing", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 0))
    })
    .await;
    assert!(unfocused.viewer.is_some());

    // With focus on the slot: the arrow moves the viewer.
    let ack = h
        .dispatch(UiAction::FocusSlot { slot_id: slot })
        .await
        .expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "focusing the slot: {ack:?}"
    );
    let focused = snapshot_until(&h, &mut sub, "the preview slot with focus", |s| {
        s.layout
            .placements
            .iter()
            .any(|p| p.slot_id == slot && p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .then(|| s.clone())
    })
    .await;
    assert_eq!(focused.focus, Some(slot));
    let ack = h.dispatch(press("ArrowDown")).await.expect("host alive");
    assert!(
        matches!(ack, ActionAck::Applied { .. }),
        "the arrow in the viewer: {ack:?}"
    );
    let moved = snapshot_until(&h, &mut sub, "the docked viewer moved down a line", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 1))
    })
    .await;
    assert_eq!(moved.viewer.expect("viewer").first_line, 1);

    // The wheel, via the host.
    h.dispatch(UiAction::PreviewScroll {
        slot_id: slot,
        delta: 3,
    })
    .await
    .expect("host alive");
    snapshot_until(&h, &mut sub, "the wheel went down three more", |s| {
        preview_de(s).filter(|p| p.viewer.as_ref().is_some_and(|v| v.first_line == 4))
    })
    .await;

    // `viewer.close` (Esc in the viewer's keymap) returns focus to the
    // listing and leaves the slot where it is.
    h.dispatch(press("Escape")).await.expect("host alive");
    let returned = snapshot_until(&h, &mut sub, "focus returned to the listing", |s| {
        let active = s
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(norte_ui_host::dto::SlotRole::Active))
            .map(|p| p.slot_id);
        (active.is_some() && active != Some(slot)).then(|| s.clone())
    })
    .await;
    assert!(preview_de(&returned).is_some(), "the slot is still open");
}

/// The menu REOPENS where it was, not at the first one.
///
/// Always opening at the first one forces the whole bar to be walked on each
/// gesture, and someone using two entries from the same menu pays for it
/// every time.
#[tokio::test]
async fn the_menu_reopens_where_it_left_off() {
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // Open, move two menus to the right and close with `Escape`.
    h.dispatch(UiAction::MenuOpen { menu: 2 })
        .await
        .expect("host alive");
    h.dispatch(press("Escape")).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(
        next_snapshot(&mut sub).await.menu.open,
        None,
        "closed entirely"
    );

    // And reopening it comes out at the same one.
    run_by_palette(&h, &mut sub, "app.menu").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(next_snapshot(&mut sub).await.menu.open, Some(2));
}

/// Alt alone (bridge 68) opens the menu like `app.menu`, and again folds it.
#[tokio::test]
async fn alt_alone_opens_and_folds_the_menu() {
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    h.dispatch(UiAction::MenuToggle).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(next_snapshot(&mut sub).await.menu.open, Some(0), "open");

    h.dispatch(UiAction::MenuToggle).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    assert_eq!(next_snapshot(&mut sub).await.menu.open, None, "folded");
}

/// With a dialog in front, Alt alone opens nothing: F9 there is eaten by the
/// dialog, and a menu on top of a pending question would fight it for the
/// keyboard.
#[tokio::test]
async fn alt_alone_does_not_open_the_menu_over_a_dialog() {
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.mkdir").await;
    snapshot_until(&h, &mut sub, "the dialog is open", |s| {
        (!s.dialogs.is_empty()).then_some(())
    })
    .await;

    h.dispatch(UiAction::MenuToggle).await.expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(snapshot.menu.open, None, "the menu does not open");
    assert!(!snapshot.dialogs.is_empty(), "the dialog remains");
}

/// Choosing in the menu runs the command, and the menu closes BEFORE.
///
/// Order matters: the command can open another screen, and doing so behind
/// the menu would leave it eating the keys of the one that just opened. It
/// is the same rule as the palette.
#[tokio::test]
async fn the_menu_choice_runs_and_the_menu_closes_first() {
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    // The "Help" menu and its first entry, which is `app.help`: it opens a
    // screen, so it serves to see that the menu does not stay on top.
    let help = norte_frontend::menu::MENUS.len() - 1;
    h.dispatch(UiAction::MenuOpen {
        menu: u32::try_from(help).expect("fits"),
    })
    .await
    .expect("host alive");
    h.dispatch(UiAction::MenuActivateRow { row: 0 })
        .await
        .expect("host alive");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(snapshot.menu.open, None, "the menu closed");
    assert!(snapshot.help.is_some(), "and what was chosen ran");
}

/// The PROCESSES panel is exited with the same key it was entered with.
///
/// A ring that enters a panel and does not leave it is not a ring: it is a
/// trap, and the reader is left with no way back to the listing without a
/// mouse.
#[tokio::test]
async fn tabbing_exits_the_processes_panel() {
    use norte_ui_host::dto::SlotRole;
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "layout.processes").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let open = next_snapshot(&mut sub).await;
    let processes = open
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Processes { slot_id, .. } => Some(*slot_id),
            _ => None,
        })
        .expect("the panel is on screen");

    let active = |s: &norte_ui_host::ViewSnapshot| {
        s.layout
            .placements
            .iter()
            .find(|p| p.role == Some(SlotRole::Active))
            .map(|p| p.slot_id)
    };
    // Walk the screen until landing on the processes panel...
    let mut inside = false;
    for _ in 0..6 {
        h.dispatch(key_alt("o")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if active(&next_snapshot(&mut sub).await) == Some(processes) {
            inside = true;
            break;
        }
    }
    assert!(inside, "the ring reaches the processes panel");

    // ...and it leaves the same way it came in.
    h.dispatch(key_alt("o")).await.expect("host alive");
    let outside = snapshot_until(&h, &mut sub, "focus left the panel", |snapshot| {
        active(snapshot).filter(|id| *id != processes)
    })
    .await;
    assert_ne!(
        outside, processes,
        "and it LEAVES it: a ring that enters and does not leave is a trap"
    );

    // And Tab also gets you out, even without entering: it goes back to a
    // LISTING. That is what guarantees no combination leaves the reader
    // stuck inside.
    let mut back = false;
    for _ in 0..6 {
        h.dispatch(key_alt("o")).await.expect("host alive");
        h.dispatch(UiAction::Resync).await.expect("host alive");
        if active(&next_snapshot(&mut sub).await) == Some(processes) {
            back = true;
            break;
        }
    }
    assert!(back, "back inside the panel");
    h.dispatch(press("Tab")).await.expect("host alive");
    let snapshot = snapshot_until(
        &h,
        &mut sub,
        "`Tab` gets out of the side panel",
        |snapshot| {
            let id = active(snapshot).filter(|id| *id != processes)?;
            snapshot
                .slots
                .iter()
                .any(|v| matches!(v, SlotView::Browser(b) if b.slot_id == id))
                .then_some(id)
        },
    )
    .await;
    assert_ne!(snapshot, processes);
}

/// The keyboard ring does NOT stop at the attributes sheet.
///
/// The shared traversal (`focus_order`) carries everything FOCUSABLE, and the
/// sheet is: the roster counts it. But it does not take keys — it follows the
/// listing's cursor, and with the keyboard inside it would stop following
/// anything, which is half of #243 — so stopping there is a stop that no key
/// gets you out of: the arrows move nothing and there is nothing on screen
/// that explains it.
///
/// The TUI walks the ring with the same rule (`takes_keys` from the shared
/// registry), and a decision duplicated between frontends diverges silently
/// (ADR 0077).
#[tokio::test]
async fn the_ring_does_not_stop_at_the_attributes_sheet() {
    use norte_ui_host::dto::SlotRole;
    // Two listings, so the ring has somewhere to go when it skips the sheet:
    // with only one the correct answer is "there is no other slot".
    let (h, _snap) = host_con_layout(fake_tree(), "orthodox", (120, 40)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "layout.metadata").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let open = next_snapshot(&mut sub).await;
    let sheet = open
        .slots
        .iter()
        .find_map(|v| match v {
            SlotView::Metadata(m) => Some(m.slot_id),
            _ => None,
        })
        .expect("the sheet is on screen");

    // A full lap of the ring: the sheet must not have focus at any point
    // of it.
    for _ in 0..6 {
        run_by_palette(&h, &mut sub, "layout.focus-next").await;
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snapshot = next_snapshot(&mut sub).await;
        let active = snapshot
            .layout
            .placements
            .iter()
            .find(|p| p.role == Some(SlotRole::Active))
            .map(|p| p.slot_id);
        assert_ne!(
            active,
            Some(sheet),
            "the ring stopped at the attributes sheet, which does not take keys"
        );
    }
}

/// Tabs: open, walk, move, go to N and close (#288).
///
/// The group is carried by the SHARED model (`add_tab` wraps the slot if
/// needed, `move_tab` does not wrap around): here it is checked that the
/// gesture arrives, that the tab brought to front takes FOCUS with it —
/// working with one that is not visible is what this prevents — and that
/// the view says what is there.
#[tokio::test]
async fn tabs_open_cycle_and_close() {
    let backend = fake_tree();
    let (h, snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    assert!(snap.layout.tabs.is_empty(), "no group, no bar");

    run_by_palette(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    let group = snapshot
        .layout
        .tabs
        .first()
        .expect("there is a group")
        .clone();
    assert_eq!(group.tabs.len(), 2, "two tabs");
    assert_eq!(group.active, 1, "the new one ends up in front");
    assert_eq!(
        Some(group.tabs[1].slot_id),
        snapshot.focus,
        "and with focus: working on one that is not visible is what this prevents"
    );

    // Walking CYCLES.
    run_by_palette(&h, &mut sub, "pane.tab-next").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert_eq!(
        snapshot.layout.tabs.first().expect("group").active,
        0,
        "from the last to the first"
    );

    // Going to an N that does not exist is refused: guessing would mean
    // switching tabs on its own.
    let ack = execute_via_palette_ack(&h, &mut sub, "pane.tab-goto-9").await;
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-such-tab"),
        "{ack:?}"
    );

    // Closing the front one leaves one, and the group dissolves.
    run_by_palette(&h, &mut sub, "pane.tab-close").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = next_snapshot(&mut sub).await;
    assert!(
        snapshot.layout.tabs.is_empty(),
        "a group of one is not a group: {:?}",
        snapshot.layout.tabs
    );
}

/// ADR 0133 (the review asked for it): a tab's `×` and `+` act on ITS group,
/// not on the one that had focus. Two groups, focus on the second, and a tab
/// from the first is closed: the first dissolves and the second keeps its
/// two.
#[tokio::test]
async fn a_tab_button_acts_on_its_group_and_not_on_the_focused_one() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Two listings, one group in each.
    run_by_palette(&h, &mut sub, "layout.split-h").await;
    run_by_palette(&h, &mut sub, "pane.tab-new").await;
    run_by_palette(&h, &mut sub, "pane.switch").await;
    run_by_palette(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = snapshot_until(&h, &mut sub, "two groups", |s| {
        (s.layout.tabs.len() == 2).then(|| s.clone())
    })
    .await;
    let (a, b) = (
        snapshot.layout.tabs[0].clone(),
        snapshot.layout.tabs[1].clone(),
    );
    let focused = snapshot.focus.expect("there is focus");
    // Focus is on one of the two; the OTHER one is clicked.
    let (pressed, other) = if b.tabs.iter().any(|t| t.slot_id == focused) {
        (a, b)
    } else {
        (b, a)
    };
    let ack = h
        .dispatch(UiAction::TabAction {
            slot_id: pressed.tabs[0].slot_id,
            verb: norte_ui_host::TabVerb::Close,
        })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snapshot = snapshot_until(&h, &mut sub, "one group fewer", |s| {
        (s.layout.tabs.len() == 1).then(|| s.clone())
    })
    .await;
    let remains = &snapshot.layout.tabs[0];
    assert_eq!(
        remains.tabs.iter().map(|t| t.slot_id).collect::<Vec<_>>(),
        other.tabs.iter().map(|t| t.slot_id).collect::<Vec<_>>(),
        "the focused group stays whole; the clicked one closed"
    );
}

/// With no group, the tab commands SAY SO.
///
/// Closing the whole slot is a different command: doing it here "because
/// there were no tabs" would close what nobody asked to close.
#[tokio::test]
async fn without_a_group_tab_commands_say_so() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    for cmd in ["pane.tab-close", "pane.tab-next", "pane.tab-move-right"] {
        let ack = execute_via_palette_ack(&h, &mut sub, cmd).await;
        assert!(
            matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-tabs"),
            "{cmd}: {ack:?}"
        );
    }
}

/// A click on a tab brings it to front; against a tree that already changed,
/// it refuses instead of getting it right by chance.
#[tokio::test]
async fn a_click_on_a_tab_brings_it_to_front() {
    let backend = fake_tree();
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    run_by_palette(&h, &mut sub, "pane.tab-new").await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let group = next_snapshot(&mut sub)
        .await
        .layout
        .tabs
        .first()
        .expect("group")
        .clone();
    let first = group.tabs[0].slot_id;

    let ack = h
        .dispatch(UiAction::SelectTab { slot_id: first })
        .await
        .expect("host alive");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    // Every layout change sends its own snapshot, so the ones piling up in
    // the queue are from BEFORE: it looks for the one that already reflects
    // the click instead of reading the first one that comes out.
    let mut seen = None;
    for _ in 0..10 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let snapshot = next_snapshot(&mut sub).await;
        if snapshot.layout.tabs.first().is_some_and(|g| g.active == 0) {
            seen = Some(snapshot);
            break;
        }
    }
    let snapshot = seen.expect("the click brings the first one to front");
    assert_eq!(snapshot.focus, Some(first));

    // A slot that is not in any group: stale, not a hit.
    let ack = h
        .dispatch(UiAction::SelectTab { slot_id: 4242 })
        .await
        .expect("host alive");
    assert!(
        matches!(
            &ack,
            ActionAck::Stale {
                reason: StaleAction::Generation
            }
        ),
        "{ack:?}"
    );
}
