//! The panel a PLUGIN paints (phase 3), through where it really goes: that
//! its frame PAINTS, that a clicked zone dispatches a catalogue command,
//! that an old response does not overwrite the current one, and that a
//! third party's text gets masked.
//!
//! The last one is not theoretical: `marco_de_wire`'s first version copied
//! the wire's fields by hand and skipped the masking the styled preview did
//! do. A panel could smuggle terminal escapes through the one path that did
//! not go through `norte_frontend::ansi::span_de_wire`.

use norte_frontend::ansi::StyledSpan;
use norte_frontend::frame::{Hit, StyledFrame};
use norte_frontend::layout::{Dir, KindId, Node, Size, SlotId};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};
use norte_tui::{mouse, panelplugin, ui};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// The slot where the plugin's panel lives in all these tests.
const PANEL: SlotId = SlotId(71);

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

/// An approved, enabled plugin that contributes the `status` panel.
fn plugin_con_panel() -> norte_proto::methods::PluginInfo {
    norte_proto::methods::PluginInfo {
        id: "git".to_owned(),
        name: "Git".to_owned(),
        publisher: String::new(),
        version: "1.0.0".to_owned(),
        category: "panel".to_owned(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        panels: vec![norte_proto::methods::PluginPanelInfo {
            kind: "status".to_owned(),
            title: "Git".to_owned(),
            min_cols: None,
            min_rows: None,
        }],
        has_help: false,
        manifest_digest: None,
    }
}

/// A screen with a listing and the plugin's panel next to it.
fn app_con_panel() -> App {
    let dir = vp("file:///casa");
    let mut app = App::new(
        Pane::new(dir.clone(), Vec::new()),
        Pane::new(dir, Vec::new()),
    );
    app.kinds.insert_panels(&[plugin_con_panel()]);
    app.set_layout(Node::Split {
        dir: Dir::Horizontal,
        sizes: vec![Size::Weight(1), Size::Fixed(24)],
        children: vec![
            Node::slot(SlotId(70), KindId::browser()),
            Node::slot(PANEL, KindId::new("plugin:git:status")),
        ],
    });
    app
}

fn marco(text: &str, hits: Vec<Hit>) -> StyledFrame {
    StyledFrame::clamped(
        vec![vec![StyledSpan {
            text: text.to_owned(),
            role: None,
            fg: None,
            bg: None,
        }]],
        hits,
    )
}

fn text_of(f: Option<&StyledFrame>) -> String {
    f.map(|f| {
        f.lines
            .iter()
            .flat_map(|l| l.iter().map(|s| s.text.clone()))
            .collect::<String>()
    })
    .unwrap_or_default()
}

fn screen(app: &App, width: u16, alto: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, alto)).expect("backend");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    (0..alto)
        .flat_map(|y| (0..width).map(move |x| (x, y)))
        .map(|(x, y)| terminal.backend().buffer()[(x, y)].symbol().to_owned())
        .collect()
}

/// What the guest describes gets PAINTED, inside a frame that says which
/// panel it is.
///
/// A contributed panel's kind is not known at compile time, so it does not
/// go through the `placed_of_kind` chain its neighbors have: it resolves
/// by prefix. Without that path, the slot got placed and stayed blank.
#[test]
fn the_plugin_frame_is_painted_with_its_title() {
    let mut app = app_con_panel();
    app.panels.entry(PANEL).frame = Some(marco("rama: main", Vec::new()));

    let seen = screen(&app, 80, 16);
    assert!(seen.contains("rama: main"), "the frame paints: {seen:?}");
    assert!(seen.contains("status"), "and the title says what it is");
}

/// Clicking a zone of the frame dispatches ITS command, through the usual
/// path.
///
/// A `Hit` runs nothing on its own: it names a catalogue command and norte
/// dispatches it, so a click cannot do anything a key could not (hard rule
/// 9). What this test pins is that the coordinate math is correct — the
/// guest talks about cells INSIDE the frame — and that the command ends up
/// in the same queue as a bar button.
#[test]
fn pressing_a_panel_zone_leaves_its_command_for_dispatch() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.panels.entry(PANEL).frame = Some(marco(
        "recargar",
        vec![Hit {
            row: 0,
            col: 0,
            width: 8,
            command: "layout.focus-next".to_owned(),
            arg: None,
        }],
    ));

    let slots = ui::panel_slots(&app, area);
    let slot = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("the panel was placed");
    // The first cell INSIDE: one past the border, in both directions.
    let (col, row) = (slot.x + 1, slot.y + 1);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let after = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert!(
        matches!(after, mouse::After::PanelBar),
        "it goes through the bar's dispatch, which is its own shortcut's"
    );
    assert_eq!(
        app.pending_panel_command.as_deref(),
        Some("layout.focus-next")
    );
}

/// A zone that names a command OUTSIDE its reach runs nothing.
///
/// The plugin chooses the label and the command, and nothing ties them
/// together: a zone that says "Update" can name `pane.unpack`, which
/// copies files. The click has the same reach as a focused panel's key, not
/// one bit more.
#[test]
fn a_zone_cannot_name_a_command_outside_its_scope() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.panels.entry(PANEL).frame = Some(marco(
        "Actualizar",
        vec![Hit {
            row: 0,
            col: 0,
            width: 10,
            command: "pane.unpack".to_owned(),
            arg: None,
        }],
    ));
    let slots = ui::panel_slots(&app, area);
    let slot = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("the panel was placed");
    let (col, row) = (slot.x + 1, slot.y + 1);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let _ = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert_eq!(
        app.pending_panel_command, None,
        "a plugin does not drive the manager from a zone"
    );
}

/// Clicking the panel's BORDER does not fire the zone underneath.
///
/// The cell count protected the top-left and not the other side: on the
/// right border it gave the column just past the last interior one, so a
/// zone spanning the whole width fired when clicking the frame itself — for
/// instance, going to drag it.
#[test]
fn pressing_the_panes_border_does_not_trigger_its_zone() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.panels.entry(PANEL).frame = Some(marco(
        "ancho entero",
        vec![Hit {
            row: 0,
            col: 0,
            width: u16::MAX,
            command: "layout.focus-next".to_owned(),
            arg: None,
        }],
    ));
    let slots = ui::panel_slots(&app, area);
    let slot = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("the panel was placed");
    // The RIGHT border, at the height of the first interior row.
    let (col, row) = (slot.x + slot.width - 1, slot.y + 1);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let _ = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert_eq!(app.pending_panel_command, None, "the frame is not the zone");
}

/// A frame cell where there is NO zone runs nothing.
#[test]
fn outside_a_zone_nothing_is_dispatched() {
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let mut app = app_con_panel();
    app.panels.entry(PANEL).frame = Some(marco(
        "recargar",
        vec![Hit {
            row: 0,
            col: 0,
            width: 8,
            command: "pane.reload".to_owned(),
            arg: None,
        }],
    ));
    let slots = ui::panel_slots(&app, area);
    let slot = slots
        .iter()
        .find(|s| s.slot == PANEL)
        .expect("the panel was placed");
    // Two rows further down: inside the panel, outside the only zone.
    let (col, row) = (slot.x + 1, slot.y + 3);
    mouse::after_frame(
        &mut app,
        None,
        mouse::FrameZones {
            slots,
            ..Default::default()
        },
    );

    let ev = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: col,
        row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    let _ = mouse::handle_at(&mut app, ev, std::time::Instant::now());
    assert_eq!(app.pending_panel_command, None);
}

/// A response to an OLD request does not overwrite the current frame.
///
/// While it was in flight, the cursor could have moved: that frame
/// describes a screen that no longer is. And it does not clear the live
/// request, which has a different signature and is still the one in charge.
#[test]
fn an_old_response_does_not_stomp_on_the_current_frame() {
    let mut app = app_con_panel();
    app.panels.entry(PANEL).frame = Some(marco("lo de ahora", Vec::new()));
    let vieja = panelplugin::Signature {
        kind: "plugin:git:status".to_owned(),
        dir: vp("file:///otro"),
        cols: 22,
        rows: 4,
        cursor: None,
    };
    let viva = panelplugin::Signature {
        kind: "plugin:git:status".to_owned(),
        dir: vp("file:///casa"),
        cols: 22,
        rows: 4,
        cursor: None,
    };
    app.panels.entry(PANEL).in_flight = Some(viva);

    panelplugin::land(
        &mut app,
        PANEL,
        &vieja,
        Some(Ok(Some(norte_proto::methods::PanelFrame {
            plugin_id: "git".to_owned(),
            lines: vec![vec![norte_proto::methods::SpanWire {
                text: "lo viejo".to_owned(),
                role: None,
                fg: None,
                bg: None,
            }]],
            hits: Vec::new(),
            state: None,
        }))),
    );

    let panel = app.panels.entry(PANEL);
    assert!(panel.in_flight.is_some(), "the live request is still alive");
    assert_eq!(text_of(panel.frame.as_ref()), "lo de ahora");
}

/// The guest's text gets MASKED, and a chrome role is not granted to it.
///
/// Both gates live in `norte_frontend::ansi::span_de_wire`, and this test
/// exists because the panel once managed to skip them: copying the fields
/// by hand compiled just as well.
#[test]
fn the_guests_text_is_masked_and_chrome_is_not_granted() {
    let mut app = app_con_panel();
    let signature = panelplugin::Signature {
        kind: "plugin:git:status".to_owned(),
        dir: vp("file:///casa"),
        cols: 22,
        rows: 4,
        cursor: None,
    };
    app.panels.entry(PANEL).in_flight = Some(signature.clone());

    panelplugin::land(
        &mut app,
        PANEL,
        &signature,
        Some(Ok(Some(norte_proto::methods::PanelFrame {
            plugin_id: "git".to_owned(),
            lines: vec![vec![norte_proto::methods::SpanWire {
                text: "rama\u{1b}[31m roja".to_owned(),
                role: Some("scrollbar-slider".to_owned()),
                fg: None,
                bg: None,
            }]],
            hits: Vec::new(),
            state: Some(b"opaco".to_vec()),
        }))),
    );

    let panel = app.panels.entry(PANEL);
    let painted = text_of(panel.frame.as_ref());
    assert!(
        !painted.contains('\u{1b}'),
        "no escape reaches the screen: {painted:?}"
    );
    let rol = panel
        .frame
        .as_ref()
        .and_then(|f| f.lines.first())
        .and_then(|l| l.first())
        .and_then(|s| s.role);
    assert!(rol.is_none(), "a plugin cannot request a chrome role");
    assert_eq!(
        panel.state.as_deref(),
        Some(&b"opaco"[..]),
        "the opaque state is saved for the next one"
    );
}
