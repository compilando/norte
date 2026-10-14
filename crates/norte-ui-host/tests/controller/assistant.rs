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
