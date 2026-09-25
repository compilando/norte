use super::*;
use norte_ui_host::dto::OrganizeLineKind;

// ---------------------------------------------------------------------------
// Organizing a directory from the window (phase 8 of the WOW program).
// ---------------------------------------------------------------------------

/// Any hash, in the shape the protocol requires.
fn hash() -> norte_proto::methods::PlanHash {
    norte_proto::methods::PlanHash::parse(&"ab".repeat(32)).expect("64 lowercase hex")
}

/// A double with an organize plan ready and its token.
fn fake_with_tree(moves: &[(&str, &str)], with_token: bool) -> Arc<Fake> {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        moves
            .iter()
            .map(|(from, _)| (from.as_bytes().to_vec(), false))
            .collect::<Vec<_>>(),
    );
    f.plan_organize = Some(
        moves
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    );
    f.organize_hash = with_token.then(hash);
    Arc::new(f)
}

/// Waits for the next update that carries the organize tree.
async fn next_tree(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::OrganizeView> {
    for _ in 0..40 {
        let next = tokio::time::timeout(WAIT_MAX, sub.recv())
            .await
            .expect("an update, not a hang")
            .expect("the host is still alive");
        match next {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Organize { organize } = c {
                            return organize.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload
                    && s.organize.is_some()
                {
                    return s.organize.clone();
                }
            }
            Update::Lagged => panic!("no lag in this test"),
        }
    }
    panic!("no update ever carried the tree");
}

/// Requests the plan through the palette. WITH NO instruction prompt, unlike
/// renaming: what is asked is "look at this directory and propose a shape".
async fn request_tree(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    by_palette(h, sub, "organize").await;
}

/// The tree opens with its count, and a folder that ALREADY was there does
/// not paint as new: painting everything as new shows a more spectacular
/// plan than it is and hides that something lands inside something the
/// reader already had.
#[tokio::test]
async fn the_tree_tells_apart_what_is_created_from_what_was_already_there() {
    let mut f = Fake::default();
    // `invoices` already exists in the directory; `new` does not.
    f.put(
        "mem:///casa",
        vec![
            (b"a.pdf".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"facturas".to_vec(), true),
        ],
    );
    f.plan_organize = Some(vec![
        ("a.pdf".to_owned(), "facturas/a.pdf".to_owned()),
        ("b.txt".to_owned(), "nueva/b.txt".to_owned()),
    ]);
    f.organize_hash = Some(hash());
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_tree(&h, &mut sub).await;

    let v = next_tree(&mut sub).await.expect("opens");
    let invoices = v
        .lines
        .iter()
        .find(|l| l.text.text == "facturas")
        .expect("is there");
    assert_eq!(invoices.kind, OrganizeLineKind::ExistingDir, "{invoices:?}");
    let new = v
        .lines
        .iter()
        .find(|l| l.text.text == "nueva")
        .expect("is there");
    assert_eq!(new.kind, OrganizeLineKind::NewDir, "{new:?}");
    // And the summary counts ONE new folder, not two.
    assert!(v.summary.contains('1'), "the count: {}", v.summary);
}

/// A plan with NO token opens no review.
///
/// With no `plan_hash` there is nothing to redeem, so approving would be a
/// button that can do nothing — and showing the tree would promise it.
#[tokio::test]
async fn a_plan_with_no_token_does_not_open_the_review() {
    let backend = fake_with_tree(&[("a.pdf", "facturas/a.pdf")], false);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_tree(&h, &mut sub).await;
    settle().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(
        snap.organize.is_none(),
        "with no token there is no review: {:?}",
        snap.organize
    );
    assert!(backend.organized.lock().expect("organizados").is_empty());
}

/// Approving requires having scrolled through the WHOLE tree, and scrolling
/// with the mouse counts the same as with the keyboard.
#[tokio::test]
async fn approving_requires_having_reached_the_end() {
    // Twelve files at the root: twelve lines, more than the ten-row window.
    let moves: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.txt"), format!("b{i:02}.txt")))
        .collect();
    let refs: Vec<(&str, &str)> = moves
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = fake_with_tree(&refs, true);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_tree(&h, &mut sub).await;
    let v = next_tree(&mut sub).await.expect("opens");
    assert!(!v.seen_all, "just opened, it has not been read in full");

    // Approving now sends NOTHING.
    h.dispatch(UiAction::OrganizeDecide { approve: true })
        .await
        .expect("host alive");
    settle().await;
    assert!(
        backend.organized.lock().expect("organizados").is_empty(),
        "without reading it in full it does not apply"
    );

    // It is scrolled with the MOUSE to the end, and then it does.
    for _ in 0..12 {
        h.dispatch(UiAction::OrganizeScroll { down: true })
            .await
            .expect("host alive");
    }
    settle().await;
    h.dispatch(UiAction::OrganizeDecide { approve: true })
        .await
        .expect("host alive");
    let sent = until(&backend, "the applied plan", |f| {
        let v = f.organized.lock().expect("organizados");
        (!v.is_empty()).then(|| v.len())
    })
    .await;
    assert_eq!(sent, 1, "a single Task for the whole batch");
    let done = backend.organized.lock().expect("organizados");
    assert_eq!(done[0].2, hash(), "with the token that came WITH the plan");
    assert_eq!(done[0].1.len(), 12);
}

/// A plugin has to be GIVEN the names: it does not list directories (rule
/// 9), and with an empty list it correctly answers that it moves nothing.
///
/// This is the bug that piloting phase 8 in a real terminal uncovered: the
/// tree never opened and the bar said "the plan moves nothing" over a
/// directory with five files inside.
#[tokio::test]
async fn an_organizer_is_given_the_directorys_names() {
    let mut f = Fake::default();
    f.put(
        "mem:///casa",
        vec![(b"a.pdf".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    f.plan_organize = Some(vec![("a.pdf".to_owned(), "pdf/a.pdf".to_owned())]);
    f.organize_hash = Some(hash());
    let mut ext = crate::help::extension("org.norte.by-extension", "By extension", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "by-extension".to_owned(),
        title: "Into folders by extension".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Organizer,
    }];
    *f.plugins.lock().expect("plugins") = vec![ext];
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Through the PALETTE, which is the path to a plugin's organizer.
    h.dispatch(key_mod("p", true, false))
        .await
        .expect("host alive");
    let mut arrived = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host alive");
        let p = next_snapshot(&mut sub).await.palette.expect("open");
        if p.rows.iter().any(|r| r.text.contains("Into folders")) {
            arrived = true;
            break;
        }
    }
    assert!(arrived, "the organizer's row never arrived");
    for c in "Into folders".chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");
    let requested = until(&backend, "the request to the organizer", |f| {
        f.organizers_requests
            .lock()
            .expect("organizers")
            .first()
            .cloned()
    })
    .await;
    assert_eq!(
        requested.2,
        vec!["a.pdf".to_owned(), "b.txt".to_owned()],
        "the operand is the whole directory, not an empty list"
    );
}

/// Discarding applies nothing and leaves the review closed.
#[tokio::test]
async fn discarding_closes_and_applies_nothing() {
    let backend = fake_with_tree(&[("a.pdf", "facturas/a.pdf")], true);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_tree(&h, &mut sub).await;
    next_tree(&mut sub).await.expect("opens");

    h.dispatch(press("Escape")).await.expect("host alive");
    settle().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.organize.is_none(), "it closed and stays closed");
    assert!(backend.organized.lock().expect("organizados").is_empty());
}

/// The tree's names are proposed by a third party over names anyone wrote:
/// they get masked and it SAYS so.
#[tokio::test]
async fn a_hostile_tree_name_is_marked() {
    // From the canonical corpus, not hand-written.
    let bytes = hostile("rtl_override");
    let altered = String::from_utf8(bytes).expect("the corpus one is UTF-8");
    let target = format!("facturas/{altered}");
    let backend = fake_with_tree(&[("a.pdf", target.as_str())], true);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_tree(&h, &mut sub).await;
    let v = next_tree(&mut sub).await.expect("opens");
    let file = v
        .lines
        .iter()
        .find(|l| l.kind == OrganizeLineKind::Moved)
        .expect("is there");
    assert!(
        !file.text.text.contains('\u{202E}'),
        "masked: {:?}",
        file.text
    );
    assert!(file.text.hostile, "and marked: {:?}", file.text);
}

/// The producer can REFUSE with a reason (#332): it says so and opens
/// nothing.
#[tokio::test]
async fn a_refusing_producer_says_so_and_opens_nothing() {
    let mut f = Fake::default();
    f.put("mem:///casa", vec![(b"a.pdf".to_vec(), false)]);
    f.organize_refuses = Some("aprueba mi capacidad `location`".to_owned());
    let backend = Arc::new(f);
    let (h, _snap) = host_tree(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_tree(&h, &mut sub).await;
    settle().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.organize.is_none(), "refusing opens no review");
    let said = snap.status.message.unwrap_or_default();
    assert!(said.contains("location"), "and says why: {said}");
}

/// In a READ-ONLY window the plan is not even requested: showing a tree that
/// will not be applicable is promising work.
#[tokio::test]
async fn in_read_only_it_is_not_even_requested() {
    let backend = fake_with_tree(&[("a.pdf", "facturas/a.pdf")], true);
    let (h, _snap) = crate::reviews::host_solo_read(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    request_tree(&h, &mut sub).await;
    settle().await;
    h.dispatch(UiAction::Resync).await.expect("host alive");
    let snap = next_snapshot(&mut sub).await;
    assert!(snap.organize.is_none(), "it does not even open");
    assert!(backend.organized.lock().expect("organizados").is_empty());
}
