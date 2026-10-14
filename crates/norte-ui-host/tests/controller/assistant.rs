use super::*;

// ---------------------------------------------------------------------------
// The assistant panel in the window (ADR 0186): the same model as the
// terminal; Apply goes through the order line's executor.
// ---------------------------------------------------------------------------

use norte_proto::methods::AiChatTurn;

fn ctrl_g() -> UiAction {
    UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "g".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    })
}

fn assistant_of(s: &norte_ui_host::ViewSnapshot) -> Option<&norte_ui_host::dto::AssistantSlotView> {
    s.slots.iter().find_map(|v| match v {
        SlotView::Assistant(a) => Some(&**a),
        _ => None,
    })
}

fn with_turn(turn: Option<AiChatTurn>) -> Arc<Fake> {
    let mut f = Fake {
        ai_turn: turn,
        ..Fake::default()
    };
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    Arc::new(f)
}

async fn ask(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    text: &str,
) -> norte_ui_host::ViewSnapshot {
    h.dispatch(ctrl_g()).await.expect("host alive");
    for c in format!("@{text}").chars() {
        h.dispatch(press(&c.to_string())).await.expect("host alive");
    }
    h.dispatch(press("Enter")).await.expect("host alive");
    wait_snapshot(h, sub, "the turn lands", |s| {
        assistant_of(s).is_some_and(|a| !a.pending && a.turns.len() >= 2)
    })
    .await
}

/// `@` in the box opens the panel and the turn lands in it.
#[tokio::test]
async fn at_opens_the_panel_and_the_turn_lands() {
    let turn = AiChatTurn::Order {
        text: "sel *.txt | trash".into(),
        note: "trashes the notes".into(),
    };
    let (h, _snap) = host_tree(with_turn(Some(turn))).await;
    let mut sub = h.subscribe();
    let snap = ask(&h, &mut sub, "tira las notas").await;
    let a = assistant_of(&snap).expect("the panel is open");
    assert_eq!(a.turns[0].kind, "reader");
    assert_eq!(a.turns[1].kind, "order");
    assert_eq!(a.turns[1].text, "sel *.txt | trash");
    assert_eq!(a.turns[1].note, "trashes the notes");
}

/// Apply goes through the order line: the delete dialog asks first.
#[tokio::test]
async fn apply_opens_the_usual_confirmation() {
    let turn = AiChatTurn::Order {
        text: "sel *.txt | trash".into(),
        note: String::new(),
    };
    let (h, _snap) = host_tree(with_turn(Some(turn))).await;
    let mut sub = h.subscribe();
    let snap = ask(&h, &mut sub, "tira las notas").await;
    let slot_id = assistant_of(&snap).expect("open").slot_id;
    h.dispatch(UiAction::AssistantApply { slot_id, turn: 1 })
        .await
        .expect("host alive");
    let snap = wait_snapshot(&h, &mut sub, "the dialog opens", |s| !s.dialogs.is_empty()).await;
    assert!(!snap.dialogs.is_empty(), "the trash asks first");
}

/// Review Focus 5 in the window: after the active pane changes — a click, no
/// `cd` — the order says it was written for another folder.
#[tokio::test]
async fn an_order_written_elsewhere_says_so_after_the_focus_moves() {
    let turn = AiChatTurn::Order {
        text: "sel *.txt | cp >other".into(),
        note: String::new(),
    };
    let mut f = Fake {
        ai_turn: Some(turn),
        ..Fake::default()
    };
    f.put(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    f.put("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    let (h, _snap) = super::transfers::two_panes_with_separate_destination(Arc::new(f)).await;
    let mut sub = h.subscribe();
    let snap = ask(&h, &mut sub, "copia las notas").await;
    assert_eq!(assistant_of(&snap).expect("open").turns[1].written_for, "");
    h.dispatch(UiAction::FocusSlot { slot_id: 2 })
        .await
        .expect("host alive");
    let snap = wait_snapshot(&h, &mut sub, "the panel is projected again", |s| {
        assistant_of(s).is_some_and(|a| !a.turns[1].written_for.is_empty())
    })
    .await;
    assert!(
        !assistant_of(&snap).expect("open").turns[1]
            .written_for
            .is_empty()
    );
}

/// Opening the panel gives it the keyboard (the active role), as in the
/// terminal.
#[tokio::test]
async fn opening_the_panel_gives_it_the_keyboard() {
    let (h, _snap) = host_tree(with_turn(None)).await;
    let mut sub = h.subscribe();
    let snap = ask(&h, &mut sub, "hola").await;
    let id = assistant_of(&snap).expect("open").slot_id;
    let role = snap
        .layout
        .placements
        .iter()
        .find(|p| p.slot_id == id)
        .and_then(|p| p.role);
    assert_eq!(
        role,
        Some(norte_ui_host::dto::SlotRole::Active),
        "the assistant is the active slot"
    );
}

/// With no assistant the panel says how to turn it on.
#[tokio::test]
async fn no_assistant_shows_how_to_turn_it_on() {
    let (h, _snap) = host_tree(with_turn(None)).await;
    let mut sub = h.subscribe();
    let snap = ask(&h, &mut sub, "hola").await;
    let a = assistant_of(&snap).expect("open");
    assert_eq!(a.turns[1].kind, "failed");
    assert!(
        a.turns[1].text.contains("enabled = true"),
        "{}",
        a.turns[1].text
    );
}

/// Model text arrives masked: no control or bidi character reaches the DOM.
#[tokio::test]
async fn model_text_arrives_masked() {
    let turn = AiChatTurn::Answer {
        text: "ok \u{1b}]2;x\u{7} \u{202e} fin".into(),
    };
    let (h, _snap) = host_tree(with_turn(Some(turn))).await;
    let mut sub = h.subscribe();
    let snap = ask(&h, &mut sub, "hola").await;
    let text = &assistant_of(&snap).expect("open").turns[1].text;
    assert!(text.ends_with("fin"));
    assert!(
        text.chars().all(|c| !norte_encoding::is_terminal_hazard(c)),
        "{text:?}"
    );
}
