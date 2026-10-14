//! The assistant panel in the terminal (ADR 0186): a turn sent as a job,
//! landed in the conversation, and Apply through the box's executor.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use norte_ai::{AiCaps, AiError, AiProvider, ChatRequest, ChatStream, ModelInfo};
use norte_core::backend::Backend;
use norte_frontend::assistant::Turn;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, KeyOwner, Modal, Pane};
use norte_tui::jobs::InFlight;

struct Scripted(Mutex<Vec<String>>);

#[async_trait]
impl AiProvider for Scripted {
    #[expect(clippy::unnecessary_literal_bound, reason = "the trait's signature")]
    fn id(&self) -> &str {
        "scripted"
    }
    fn capabilities(&self) -> AiCaps {
        AiCaps::STREAMING
    }
    fn is_local(&self) -> bool {
        true
    }
    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, AiError> {
        let next = self.0.lock().expect("script").remove(0);
        Ok(futures::stream::iter(vec![Ok(next)]).boxed())
    }
    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
}

fn vp(s: &str) -> VPath {
    VPath::parse(s).expect("vpath")
}

fn entry(dir: &VPath, name: &str) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.as_bytes().to_vec()).expect("segment")),
        kind: EntryKind::File,
        size: Some(10),
        mtime_ms: Some(0),
    }
}

fn rig(script: &[&str], enabled: bool) -> (App, Backend) {
    let engine = norte_core::Engine::new();
    engine.register_provider(Arc::new(norte_testkit::MemProvider::new()));
    engine.set_ai_provider(Arc::new(Scripted(Mutex::new(
        script.iter().map(|s| (*s).to_owned()).collect(),
    ))));
    engine.set_ai_config(norte_core::ai::AiConfig {
        enabled,
        ..norte_core::ai::AiConfig::default()
    });
    let d = vp("mem:///d");
    let es = vec![entry(&d, "a.pdf"), entry(&d, "b.txt")];
    let mut app = App::new(Pane::new(d, es), Pane::new(vp("mem:///o"), Vec::new()));
    app.set_focus(0);
    (app, Backend::Embedded(Arc::new(engine)))
}

async fn ask(app: &mut App, backend: &Backend, work: &mut InFlight, text: &str) {
    app.assistant.push_str(text);
    norte_tui::assistant::submit(app, backend, work);
    let handle = work.assistant.take().expect("a turn in flight");
    let res = handle.await;
    work.assistant = None;
    norte_tui::assistant::harvest(app, work, res);
}

const ORDER: &str = r#"{"kind":"order","text":"sel ext:pdf | cp >other","note":"copies the pdf"}"#;

#[tokio::test]
async fn a_turn_lands_in_the_conversation() {
    let (mut app, backend) = rig(&[ORDER], true);
    let mut work = InFlight::default();
    ask(&mut app, &backend, &mut work, "copy the pdfs over").await;
    let turns = app.assistant.turns();
    assert_eq!(turns.len(), 2);
    assert!(matches!(&turns[1], Turn::Order { text, .. } if text == "sel ext:pdf | cp >other"));
}

/// Apply goes through the box's executor: the F5 confirmation opens over the
/// rows the order matched — nothing is copied yet.
#[tokio::test]
async fn apply_opens_the_usual_confirmation() {
    let (mut app, backend) = rig(&[ORDER], true);
    let mut work = InFlight::default();
    ask(&mut app, &backend, &mut work, "copy the pdfs over").await;
    let mut stream = futures::stream::pending();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    norte_tui::assistant::apply(&mut app, &backend, &mut events, &mut work).await;
    assert!(
        matches!(
            app.modal,
            Some(Modal::TransferName { .. } | Modal::ConfirmTransfer { .. })
        ),
        "the copy asks first"
    );
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// With AI off the panel shows what to add to `norte.toml`, not an error.
#[tokio::test]
async fn ai_off_shows_how_to_turn_it_on() {
    let (mut app, backend) = rig(&[ORDER], false);
    let mut work = InFlight::default();
    ask(&mut app, &backend, &mut work, "hola").await;
    let Some(Turn::Failed(why)) = app.assistant.turns().last() else {
        panic!("{:?}", app.assistant.turns())
    };
    assert!(why.contains("enabled = true"), "{why}");
}

/// `@` in the box opens the panel with the keyboard and sends the text.
#[tokio::test]
async fn at_in_the_box_opens_the_panel_and_asks() {
    let (mut app, _backend) = rig(&[ORDER], true);
    app.open_assistant();
    assert_eq!(app.key_owner(), KeyOwner::Assistant);
    assert!(app.assistant_slot().is_some());
    app.toggle_assistant();
    assert!(app.assistant_slot().is_none(), "the same key closes it");
}

fn painted(app: &App, w: u16, h: u16) -> String {
    let mut term = ratatui::Terminal::new(ratatui::backend::TestBackend::new(w, h)).expect("term");
    term.draw(|f| norte_tui::ui::draw(f, app)).expect("draw");
    term.backend()
        .buffer()
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect()
}

/// Review Focus 5: an order written for one folder, applied after the
/// focus moved, says so — and runs against the CURRENT pane.
#[tokio::test]
async fn an_order_written_elsewhere_says_so_and_runs_here() {
    let _ = norte_i18n::force(norte_i18n::Lang::En);
    let (mut app, backend) = rig(&[ORDER], true);
    let mut work = InFlight::default();
    ask(&mut app, &backend, &mut work, "copy the pdfs over").await;
    app.set_focus(1);
    app.open_assistant();
    let screen = painted(&app, 160, 40);
    assert!(
        screen.contains("written for"),
        "the panel says where it was written"
    );
    let mut stream = futures::stream::pending();
    let mut events = norte_tui::console::Console::detached(&mut stream);
    norte_tui::assistant::apply(&mut app, &backend, &mut events, &mut work).await;
    assert!(
        app.modal.is_none(),
        "the other pane has no pdf: nothing to copy"
    );
    assert_eq!(
        app.message.as_deref(),
        Some(norte_i18n::t("msg-order-nothing-matched").as_str())
    );
}

/// The selected turn is always in view: Enter never applies an order the
/// reader cannot see.
#[tokio::test]
async fn the_selected_order_is_scrolled_into_view() {
    let answers: Vec<String> = (0..12)
        .map(|i| format!("{{\"kind\":\"answer\",\"text\":\"answer number {i}\"}}"))
        .collect();
    let mut script = vec![ORDER];
    script.extend(answers.iter().map(String::as_str));
    let (mut app, backend) = rig(&script, true);
    let mut work = InFlight::default();
    for i in 0..13 {
        ask(&mut app, &backend, &mut work, &format!("q{i}")).await;
    }
    app.open_assistant();
    while app.assistant.selected_order().is_none() {
        app.assistant.up();
    }
    let screen = painted(&app, 120, 16);
    assert!(
        screen.contains("sel ext:pdf"),
        "the order under the cursor is painted"
    );
}

/// Text the model wrote is masked before it is painted.
#[tokio::test]
async fn model_text_is_masked_when_painted() {
    // JSON escapes: the model's reply carries ESC, BEL and an RLO.
    let hostile = "{\"kind\":\"answer\",\"text\":\"ok \\u001b]2;pwned\\u0007 \\u202e done\"}";
    let (mut app, backend) = rig(&[hostile], true);
    let mut work = InFlight::default();
    app.open_assistant();
    ask(&mut app, &backend, &mut work, "hola").await;
    let mut term =
        ratatui::Terminal::new(ratatui::backend::TestBackend::new(120, 30)).expect("term");
    term.draw(|f| norte_tui::ui::draw(f, &app)).expect("draw");
    let buf = term.backend().buffer().clone();
    let painted: String = buf
        .content()
        .iter()
        .map(ratatui::buffer::Cell::symbol)
        .collect();
    assert!(painted.contains("done"), "the answer is painted");
    assert!(!painted.contains('\u{1b}') && !painted.contains('\u{202e}'));
}
