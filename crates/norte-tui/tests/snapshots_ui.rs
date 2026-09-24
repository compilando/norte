//! Render snapshots (phase 10, insta): the EXACT shape of each screen is
//! frozen — any visual change is a conscious diff (`cargo insta review`).
//! Deterministic: fixed language, fixed data.

use norte_core::TransferOptions;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Modal, Pane, Trail, TransferKind, sort_entries};
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn entry(dir: &VPath, name: &[u8], kind: EntryKind, size: Option<u64>) -> Entry {
    Entry {
        attrs: std::collections::BTreeMap::new(),
        path: dir.join(Segment::new(name.to_vec()).unwrap()),
        kind,
        size,
        mtime_ms: None,
    }
}

fn render(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

/// Like [`render`] but at 80×24 (MAJOR-1 item d, H1 close): the extension
/// manager and the theme picker need more visible rows than the 16-row
/// screen used in the rest of the file, to paint their full list without
/// vertical clipping.
fn render_80x24(app: &App) -> String {
    render_at(app, 80, 24)
}

/// The same paint at a GIVEN size: whatever degrades with width (the
/// settings section index, for example) cannot be checked at just one.
fn render_at(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

/// H1 T3 (#24): overlay hints are no longer static — they are precomputed
/// from the effective `dialog` in force (`main.rs`, `DialogHints::build`).
/// The render tests build `App` directly (without going through `main`), so
/// they replicate the SAME computation with the real `orthodox` preset: the
/// snapshot freezes what the user would really see, not an empty string.
fn default_dialog_hints() -> norte_tui::hints::DialogHints {
    use norte_tui::keymap::{COMMANDS, DIALOG_COMMANDS, Effective, Screen, presets};
    let (_, preset) = presets()
        .into_iter()
        .find(|(n, _)| *n == "orthodox")
        .expect("orthodox preset");
    let known: Vec<&str> = COMMANDS
        .iter()
        .copied()
        .chain(DIALOG_COMMANDS.iter().copied())
        .collect();
    let eff = Effective::build_for(&preset, &[], &known, Screen::Dialog).expect("effective dialog");
    norte_tui::hints::DialogHints::build(&eff)
}

fn app_base() -> App {
    let left = vp("file:///casa");
    let right = vp("file:///otro");
    let mut entries = vec![
        entry(&left, b"docs", EntryKind::Dir, None),
        entry(&left, b"src", EntryKind::Dir, None),
        entry(&left, b"notas.txt", EntryKind::File, Some(420)),
        entry(
            &left,
            &[0xE9, b'.', b'd', b'a', b't'],
            EntryKind::File,
            Some(7),
        ),
        entry(&left, b"enlace", EntryKind::Symlink, None),
    ];
    sort_entries(&mut entries);
    let mut app = App::new(
        Pane::new(left, entries),
        Pane::new(
            right.clone(),
            vec![entry(&right, b"cosa", EntryKind::File, Some(1))],
        ),
    );
    app.focused_mut().move_down(1);
    app.dialog_hints = default_dialog_hints();
    app
}

/// #210: in the status bar, the PATH yields, and the `pos/total` counter does
/// not.
///
/// With a long path, ratatui used to clip the tail: what stayed visible was a
/// stray digit of the total, which reads like anything else. Now the path is
/// ellipsized in the middle — the start says where you are and the end which
/// directory it is — and everything that follows survives whole.
/// #149: the space warning is painted BELOW the destination and ABOVE the
/// keys — the last thing read before deciding.
///
/// And only when there is one: that it fits, that the destination cannot say
/// how much room is left, or that how much is about to move is unknown, all
/// three stay silent, because a "yes it fits" on every copy teaches you not
/// to read the line.
#[test]
fn the_transfer_modal_paints_the_space_warning() {
    let dir = vp("file:///casa");
    let paint = |space: Option<String>| {
        let mut app = App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        );
        app.dialog_hints = default_dialog_hints();
        app.modal = Some(Modal::ConfirmTransfer {
            kind: TransferKind::Copy,
            items: vec![vp("file:///casa/a.bin"), vp("file:///casa/b.bin")],
            to: vp("file:///medios"),
            space,
            confine: None,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        terminal.backend().to_string()
    };

    let notice = norte_frontend::space::warning(
        Some(4_200_000_000),
        Some(1_100_000_000),
        norte_i18n::active(),
    )
    .expect("does not fit: there is a warning");
    let con = paint(Some(notice.clone()));
    let lines: Vec<&str> = con.lines().collect();
    let row = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in:\n{con}"))
    };
    let dest = row("medios");
    let warned = row(notice.split_whitespace().next().expect("first word"));
    assert!(
        warned > dest,
        "the warning goes below the destination:\n{con}"
    );

    // Without a warning, no trace of it.
    let sin = paint(None);
    assert!(
        !sin.contains(&notice),
        "when it fits, nothing is said:\n{sin}"
    );
}

/// #164: and below the space one, the confinement one — same class of line
/// (a fact about the destination, before saying yes) and the same contract:
/// only when there is one, and without blocking anything.
#[test]
fn the_transfer_modal_paints_the_confinement_warning() {
    let dir = vp("file:///casa");
    let paint = |space: Option<String>, confine: Option<String>| {
        let mut app = App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        );
        app.dialog_hints = default_dialog_hints();
        app.modal = Some(Modal::ConfirmTransfer {
            kind: TransferKind::Copy,
            items: vec![vp("file:///casa/a.bin")],
            to: vp("sftp://host/medios"),
            space,
            confine,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        terminal.backend().to_string()
    };

    let unconfined = norte_frontend::confine::warning(
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        },
        norte_i18n::active(),
    )
    .expect("a destination that does not confine says so");
    let space_msg = norte_frontend::space::warning(
        Some(4_200_000_000),
        Some(1_100_000_000),
        norte_i18n::active(),
    )
    .expect("does not fit: there is a warning");

    let con = paint(Some(space_msg.clone()), Some(unconfined.clone()));
    let lines: Vec<&str> = con.lines().collect();
    let row = |needle: &str| {
        lines
            .iter()
            .position(|l| l.contains(needle))
            .unwrap_or_else(|| panic!("missing {needle:?} in:\n{con}"))
    };
    let dest = row("medios");
    let space_row = row(space_msg.split_whitespace().next().expect("word"));
    // The modal wraps, so we look for a word the line does not share with any
    // other instead of the whole phrase.
    let confine_row = row("symlink");
    assert!(space_row > dest, "space below the destination:\n{con}");
    assert!(
        confine_row > space_row,
        "and confinement below space:\n{con}"
    );

    // A destination that does confine says nothing, which is the normal case.
    let sin = paint(None, None);
    assert!(
        !sin.contains("symlink"),
        "one that confines does not announce itself:\n{sin}"
    );
}

/// #343, the other half: copying ONE file REQUIRES both checks.
///
/// Painting them is useless if nobody builds them. The split by item count
/// left `pending_dest_check` unset on the path of a single file, so the loop
/// had nobody to ask and the two lines were never filled in.
#[test]
fn copying_a_single_file_requests_the_destination_check() {
    let dir = vp("file:///casa");
    let the_entry = entry(&dir, b"a.bin", EntryKind::File, Some(4_200_000_000));
    let mut app = App::new(
        Pane::new(dir.clone(), vec![the_entry]),
        Pane::new(vp("file:///medios"), Vec::new()),
    );
    app.panes[0].set_cursor(0);

    app.open_transfer_to_dir(TransferKind::Copy, 0, vp("file:///medios"), Some(0));

    let check = app
        .pending_dest_check
        .as_ref()
        .expect("a single file also asks about its destination");
    assert_eq!(check.to, vp("file:///medios"));
    assert_eq!(
        check.total,
        Some(4_200_000_000),
        "and with the size of THAT file, to be able to say whether it fits"
    );
    assert!(matches!(app.modal, Some(Modal::TransferName { .. })));
}

/// #343: copying a SINGLE file warns the same as copying several.
///
/// The terminal used to split by item count: with several it opened
/// `ConfirmTransfer` and built the destination check, and with just one it
/// opened the NAME dialog and built nothing. So copying a single file said
/// neither "does not fit" nor "this destination does not hold its writes",
/// while the window did say so.
///
/// The confinement one is the one whose absence is most troubling: its
/// absence MEANS the destination holds its writes, so silencing it asserts
/// something nobody checked. And since #219 that also covers a single leaf.
#[test]
fn the_name_modal_paints_the_destination_warnings() {
    let dir = vp("file:///casa");
    let paint = |space: Option<String>, confine: Option<String>| {
        let mut app = App::new(
            Pane::new(dir.clone(), Vec::new()),
            Pane::new(dir.clone(), Vec::new()),
        );
        app.dialog_hints = default_dialog_hints();
        app.modal = Some(Modal::TransferName {
            kind: TransferKind::Copy,
            from: vp("file:///casa/a.bin"),
            to_dir: vp("sftp://host/medios"),
            name: "a.bin".to_owned(),
            original: b"a.bin".to_vec(),
            touched: false,
            from_marks: false,
            enc: None,
            error: None,
            space,
            confine,
        });
        let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        terminal.backend().to_string()
    };

    let unconfined = norte_frontend::confine::warning(
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::empty(),
            max_path: None,
        },
        norte_i18n::active(),
    )
    .expect("a destination that does not confine says so");
    let space_msg = norte_frontend::space::warning(
        Some(4_200_000_000),
        Some(1_100_000_000),
        norte_i18n::active(),
    )
    .expect("does not fit: there is a warning");

    let con = paint(Some(space_msg.clone()), Some(unconfined));
    assert!(
        con.contains(space_msg.split_whitespace().next().expect("word")),
        "the space warning shows up with a single file:\n{con}"
    );
    assert!(
        con.contains("symlink"),
        "and so does the confinement one:\n{con}"
    );

    let sin = paint(None, None);
    assert!(
        !sin.contains("symlink"),
        "and when there is nothing to say, nothing is said:\n{sin}"
    );
}

#[test]
fn the_status_bar_clips_the_path_and_not_the_counter() {
    let hondo = vp(&format!(
        "file:///{}",
        ["carpeta-con-nombre-larguisimo"; 6].join("/")
    ));
    let mut app = App::new(
        Pane::new(
            hondo.clone(),
            (0..42)
                .map(|i| {
                    entry(
                        &hondo,
                        format!("f{i:03}.txt").as_bytes(),
                        EntryKind::File,
                        Some(1),
                    )
                })
                .collect(),
        ),
        Pane::new(hondo, Vec::new()),
    );
    app.focused_mut().set_cursor(7);

    let mut terminal = Terminal::new(TestBackend::new(80, 24)).expect("terminal");
    ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, 80, 24));
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let bar = terminal
        .backend()
        .to_string()
        .lines()
        .last()
        .expect("status bar")
        .to_owned();

    assert!(
        bar.contains("8/42"),
        "the whole counter, which is what says how many there are: {bar:?}"
    );
    assert!(
        bar.contains('…'),
        "and the path yields in the middle: {bar:?}"
    );
}

#[test]
fn snapshot_navigation() {
    insta::assert_snapshot!(render(&app_base()));
}

/// G3b (ADR 0037): plugin decorator badge, AFTER the hostile-name badge slot
/// — `docs` carries a badge with a recognized ROLE (theme color); `src`
/// carries a HOSTILE badge (embedded control, longer than the 8-char cap)
/// that must arrive already MASKED and TRUNCATED (never the raw control,
/// never more than 8 chars); `notes.txt` carries no decoration — its row
/// looks exactly as it did before G3b (no extra span).
#[test]
fn snapshot_plugin_decoration_masked_hostile_badge() {
    let mut app = app_base();
    let pane = app.focused_mut();
    let by_name = |entries: &[Entry], name: &[u8]| -> VPath {
        entries
            .iter()
            .find(|e| e.path.file_name().is_some_and(|n| n.as_bytes() == name))
            .expect("fixture entry")
            .path
            .clone()
    };
    let entries = pane.entries().to_vec();
    let mut decorations = std::collections::HashMap::new();
    decorations.insert(
        by_name(&entries, b"docs"),
        norte_frontend::sanitize_decoration(&norte_proto::methods::DecorationWire {
            badge: Some("M".to_string()),
            role: Some("warning".to_string()),
        }),
    );
    decorations.insert(
        by_name(&entries, b"src"),
        norte_frontend::sanitize_decoration(&norte_proto::methods::DecorationWire {
            badge: Some("A\nBCDEFGHIJ".to_string()),
            role: None,
        }),
    );
    pane.set_decorations(decorations);
    insta::assert_snapshot!(render(&app));
}

/// ADR 0105: the ICON column, to the left of the name. `src` carries an
/// icon; `docs` carries an icon AND a badge — both slots in one row —;
/// `notes.txt` carries no icon, and still carries the SLOT, so its name
/// stays aligned with the rest. The "Name" header shifts the same amount.
#[test]
fn snapshot_icon_column_to_the_left_of_the_name() {
    let mut app = app_base();
    let pane = app.focused_mut();
    let by_name = |entries: &[Entry], name: &[u8]| -> VPath {
        entries
            .iter()
            .find(|e| e.path.file_name().is_some_and(|n| n.as_bytes() == name))
            .expect("fixture entry")
            .path
            .clone()
    };
    let entries = pane.entries().to_vec();
    let mut decorations = std::collections::HashMap::new();
    decorations.insert(
        by_name(&entries, b"src"),
        norte_frontend::sanitize_icon(&norte_proto::methods::DecorationWire {
            badge: Some("📁".to_string()),
            role: None,
        }),
    );
    let mut docs = norte_frontend::sanitize_decoration(&norte_proto::methods::DecorationWire {
        badge: Some("M".to_string()),
        role: Some("warning".to_string()),
    });
    docs.icon = Some("📁".to_string());
    decorations.insert(by_name(&entries, b"docs"), docs);
    pane.set_decorations(decorations);
    insta::assert_snapshot!(render(&app));
}

/// Quick search in filter mode (spec 2026-07-18): the left pane lists ONLY
/// the matches, with the input line `/{query} n/m` at the foot and the
/// cursor on the filtered selection; the right one stays intact.
#[test]
fn snapshot_quick_search_filter() {
    let mut app = app_base();
    let pane = app.focused_mut();
    pane.quick_start(norte_tui::nav::Mode::Filter);
    pane.quick_char('s');
    insta::assert_snapshot!(render(&app));
}

/// Live-search dialog (`Alt+F7`, liveSearch T6): name field with text
/// (cursor `_`), regex/case toggles and the walk root. The `cwd` carries a
/// bidi override: it comes out MASKED and with the badge (never raw bidi at
/// the edge, spec §6) — checks the modal's sanitizing.
#[test]
fn snapshot_search_dialog() {
    let mut app = app_base();
    // Sets the pane-with-focus's hostile cwd by rebuilding it (same listing
    // and cursor as `app_base`): `Pane` no longer exposes `dir` as a field —
    // its pure state lives in `norte_frontend::PaneState` (#82).
    let entries = app.panes[0].entries().to_vec();
    app.panes[0] = Pane::new(vp("file:///casa/evil%E2%80%AEdir"), entries);
    app.panes[0].move_down(1);
    app.open_search_dialog();
    let dialog = app.search_dialog.as_mut().expect("dialog open");
    for c in "*.rs".chars() {
        dialog.push_char(c);
    }
    insta::assert_snapshot!(render(&app));
}

/// Live-search virtual pane (`Alt+F7`, liveSearch T6): the pane with focus
/// lists the HITS as they arrive (plain name, full `VPath` under the hood)
/// and the bar paints `search-status-running` ("searching…"); the other pane
/// stays normal.
#[test]
fn snapshot_search_pane_virtual() {
    let mut app = app_base();
    let root = vp("file:///casa");
    let pane = app.focused_mut();
    pane.begin_search(root.clone());
    pane.extend_listing(vec![
        entry(
            &vp("file:///casa/src"),
            b"main.rs",
            EntryKind::File,
            Some(120),
        ),
        entry(
            &vp("file:///casa/docs"),
            "a\u{00F1}o.txt".as_bytes(),
            EntryKind::File,
            Some(88),
        ),
    ]);
    insta::assert_snapshot!(render(&app));
}

/// History popup (spec 2026-07-18, `Alt+↓`): dirs of the pane with focus,
/// most recent first, with the cursor at the top.
#[test]
fn snapshot_popup_history() {
    let mut app = app_base();
    app.history[0].push(vp("file:///casa/docs"));
    app.history[0].push(vp("file:///proyectos"));
    app.open_nav_popup(norte_tui::app::NavPopupKind::History);
    insta::assert_snapshot!(render(&app));
}

/// Hotlist popup (`Ctrl+D`): a valid entry with `name — path`, an INVALID
/// entry with its warning (per-entry degradation, no crash), and the key
/// footer `[enter]/[a]/[d]/[esc]`.
#[test]
fn snapshot_popup_hotlist() {
    let mut app = app_base();
    app.hotlist = vec![
        norte_tui::config::HotlistItem {
            name: "trabajo".into(),
            target: Ok(vp("file:///home/o/work")),
        },
        norte_tui::config::HotlistItem {
            name: "rota".into(),
            target: Err("err-invalid-path".into()),
        },
    ];
    app.open_nav_popup(norte_tui::app::NavPopupKind::Hotlist);
    insta::assert_snapshot!(render(&app));
}

/// MAJOR-1 item (d), H1 close: at 80 columns, the theme picker's GENERATED
/// hint (`app.theme`, F9) no longer cuts off mid-word — the box now grows
/// with its footer (see `ui::draw_theme_picker`). It is pinned AND checked
/// live that no label ended up split.
#[test]
fn snapshot_theme_picker_80x24() {
    let mut app = app_base();
    app.open_theme_picker();
    let text = render_80x24(&app);
    insta::assert_snapshot!(text.clone());
    let hint = &app.dialog_hints.picker;
    assert!(
        !hint.is_empty(),
        "the orthodox preset binds confirm/cancel to the picker"
    );
    assert!(
        text.contains(hint.as_str()),
        "the generated hint must fit WHOLE, with no cuts: hint={hint:?}\n{text}"
    );
}

/// #108 7a: the columns picker at 80×24 — checkbox per row, sort arrow on
/// the current column, and a row with an OPAQUE hostile id (user config with
/// an embedded RLO) painted MASKED, never raw (#73). Same noisy guarantees
/// as `snapshot_theme_picker_80x24`: the generated hint present and whole
/// (no silent footer truncation).
#[test]
fn snapshot_columns_picker_80x24() {
    let mut app = app_base();
    // Hostile config BEFORE opening: the picker starts from the resolved set.
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec![
            "name".into(),
            "size".into(),
            "mtime".into(),
            "attr:x\u{202E}evil".into(),
        ]),
        ..Default::default()
    };
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    app.open_columns_picker(&[]);
    let text = render_80x24(&app);
    let hint = &app.dialog_hints.columns;
    assert!(
        !hint.is_empty(),
        "the orthodox preset binds toggle/sort/confirm/cancel to the picker"
    );
    assert!(
        text.contains(hint.as_str()),
        "the generated hint must fit WHOLE, with no cuts: hint={hint:?}\n{text}"
    );
    assert!(
        !text.contains('\u{202E}'),
        "the config's RLO never reaches the terminal raw:\n{text}"
    );
    insta::assert_snapshot!(text);
}

/// Phase A: the layout picker at 80×24. The five factory ones with their
/// provenance, the DRAWN preview of the tree's split — not a saved drawing —
/// and the note that choosing a layout does not touch the keys, which shows
/// up because `orthodox` is also the name of a keymap preset.
///
/// This picker's state already had tests; what got PAINTED did not, and
/// that is how an invisible dialog that held onto the keyboard could open.
/// This snapshot is the door that was missing.
#[test]
fn snapshot_layout_picker_80x24() {
    let mut app = app_base();
    app.open_layout_picker(Vec::new());
    let text = render_80x24(&app);
    let hint = &app.dialog_hints.picker;
    assert!(!hint.is_empty(), "the orthodox preset binds confirm/cancel");
    assert!(
        text.contains("orthodox") && text.contains("full"),
        "the five factory ones are offered:\n{text}"
    );
    assert!(
        text.contains(&norte_i18n::t("layout-picker-keymap-note")),
        "the keymap note fits whole under the row that deserves it:\n{text}"
    );
    insta::assert_snapshot!(text);
}

/// #117 encoding-audit L1: a MILE-LONG config id that fails to parse is
/// shown in the picker CAPPED at `HEADER_MAX_CHARS` (GUI parity) — without
/// the cap the whole overlay would widen up to the frame for a single id.
#[test]
fn picker_caps_a_mile_long_opaque_id() {
    let mut app = app_base();
    let mile_long = "x".repeat(60); // does not parse: neither builtin nor attr:/plugin:
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".into(), mile_long.clone()]),
        ..Default::default()
    };
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    app.open_columns_picker(&[]);
    let text = render_80x24(&app);
    let cap = norte_frontend::columns::HEADER_MAX_CHARS;
    assert!(
        text.contains(&"x".repeat(cap)),
        "the capped row must show:\n{text}"
    );
    assert!(
        !text.contains(&"x".repeat(cap + 1)),
        "never more than {cap} chars of the opaque id:\n{text}"
    );
}

/// #108 7b: `[[ui.columns.spec]]` live in the pane — `size` with SI format
/// ("1.5 kB", not "1.5 KiB"), custom header `Peso` (replaces "Tamaño") and
/// fixed width 9; `kind` aligned LEFT (content after the separator, padding
/// to the right — the right default stays pinned by `snapshot_navigation`).
/// The hostile header is not re-pinned here: the choke point is
/// `ColumnsSettings::resolve` (unit test in norte-frontend).
#[test]
fn snapshot_columns_spec_size_si_header_custom_kind_left() {
    let left = vp("file:///casa");
    let right = vp("file:///otro");
    let mut entries = vec![
        entry(&left, b"docs", EntryKind::Dir, None),
        entry(&left, b"grande.bin", EntryKind::File, Some(1500)),
        entry(&left, b"notas.txt", EntryKind::File, Some(420)),
    ];
    sort_entries(&mut entries);
    let mut app = App::new(
        Pane::new(left, entries),
        Pane::new(
            right.clone(),
            vec![entry(&right, b"cosa", EntryKind::File, Some(1))],
        ),
    );
    app.dialog_hints = default_dialog_hints();
    // `kind` is not in the default set: explicit list — with the size
    // spec's fixed width of 9, 10+9+10+9 = 38 cells match EXACTLY inside the
    // pane and `kind` is not dropped.
    let mut cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec![
            "name".into(),
            "size".into(),
            "mtime".into(),
            "kind".into(),
        ]),
        ..Default::default()
    };
    cfg.specs.insert(
        "size".into(),
        norte_config::ColumnSpec {
            format: Some("si".into()),
            header: Some("Peso".into()),
            width: Some(norte_config::WidthChoice::Fixed(9)),
            ..Default::default()
        },
    );
    cfg.specs.insert(
        "kind".into(),
        norte_config::ColumnSpec {
            align: Some(norte_config::AlignChoice::Left),
            ..Default::default()
        },
    );
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    let text = render(&app);
    assert!(text.contains("Peso"), "the spec's custom header:\n{text}");
    assert!(
        text.contains("1.5 kB") && !text.contains("KiB"),
        "size in SI, not IEC:\n{text}"
    );
    insta::assert_snapshot!(text);
}

/// MAJOR-1 item (d): same as the theme picker, for the extension manager
/// (`app.extensions`, M4-P3) — its hint after (a)+(b) (short labels + no
/// arrows) plus (c)'s footer sizing must fit whole at 80 columns.
#[test]
fn snapshot_extensions_80x24() {
    let mut app = app_base();
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: vec!["fs-read".into()],
            approved: false,
            enabled: true,
            description: None,
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }],
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    });
    let text = render_80x24(&app);
    insta::assert_snapshot!(text.clone());
    let hint = &app.dialog_hints.extensions;
    assert!(
        !hint.is_empty(),
        "the orthodox preset binds approve/toggle-enabled/cancel to extensions"
    );
    assert!(
        text.contains(hint.as_str()),
        "the generated hint must fit WHOLE, with no cuts: hint={hint:?}\n{text}"
    );
}

/// P1: a plugin's description (manifest `[plugin]`, 280-char cap) is
/// THIRD-PARTY text — a second line under the plugin's row, but hostile
/// (RTL override, `rtl_override` corpus) NEVER paints raw. Same masking
/// criterion as `render_enmascara_nombre_hostil` (`extensions.rs`), at the
/// level of a whole snapshot.
#[test]
fn snapshot_extensions_description_hostile_80x24() {
    let hostile = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("corpus fixture");
    let description = String::from_utf8_lossy(&hostile.bytes).into_owned();
    let mut app = app_base();
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "1.0.0".into(),
            category: "previewer".into(),
            capabilities: vec!["fs-read".into()],
            approved: true,
            enabled: true,
            description: Some(description),
            commands: Vec::new(),
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: false,
            manifest_digest: None,
        }],
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: None,
    });
    let text = render_80x24(&app);
    // The check is on the INJECTED CHARACTER, not "no hazard anywhere on the
    // screen" — the backend's `to_string()` joins lines with `\n`, which
    // `is_terminal_hazard` (correctly) also flags as control: a blind check
    // over the WHOLE render would give a false positive from the buffer's
    // own formatting, not from filtered hostile text.
    assert!(
        !text.contains('\u{202E}'),
        "the description's RTL override painted raw: {text}"
    );
    assert!(
        text.contains('\u{FFFD}'),
        "the hostile description must be masked to U+FFFD: {text}"
    );
    insta::assert_snapshot!(text);
}

/// G3c: a plugin's `[config]` panel (drill-down from the extension manager)
/// — two keys (`bool` selected, `enum` with a HOSTILE description) paint
/// with no raw bytes, the selected one highlighted.
#[test]
/// ADR 0104, parity with the window: the manager in two columns, and the
/// CHOSEN extension's settings inside its card, with the cursor on the key
/// and the key's description below. The commands it contributes, at the
/// foot of the card.
fn snapshot_extensions_card_with_settings_80x24() {
    use norte_frontend::plugin_config::{PluginConfigState, sanitize_config_keys};
    let mut app = app_base();
    let wire_keys = vec![norte_proto::methods::PluginConfigKeyWire {
        key: "style".into(),
        kind: "enum".into(),
        default: "emoji".into(),
        min: None,
        max: None,
        values: vec!["emoji".into(), "ascii".into()],
        description: Some("Glyph set".into()),
        value: "ascii".into(),
    }];
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: vec![norte_proto::methods::PluginInfo {
            id: "org.norte.file-icons".into(),
            name: "File icons".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "decorator".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: Some("An icon on each row.".into()),
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "reload".into(),
                title: "Reload the table".into(),
                kind: norte_proto::methods::PluginCommandKind::Command,
            }],
            columns: Vec::new(),
            panels: Vec::new(),
            has_help: true,
            manifest_digest: None,
        }],
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: Some(norte_tui::app::PluginConfigPanel {
            plugin_id: "org.norte.file-icons".into(),
            plugin_name: "File icons".into(),
            state: PluginConfigState::new(sanitize_config_keys(&wire_keys)),
        }),
    });
    insta::assert_snapshot!(render_80x24(&app));
}

#[test]
fn snapshot_plugin_config_panel_80x24() {
    use norte_frontend::plugin_config::{PluginConfigState, sanitize_config_keys};
    let hostile = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("corpus fixture");
    let desc_hostile = String::from_utf8_lossy(&hostile.bytes).into_owned();
    let mut app = app_base();
    let wire_keys = vec![
        norte_proto::methods::PluginConfigKeyWire {
            key: "verbose".into(),
            kind: "bool".into(),
            default: "false".into(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "true".into(),
        },
        norte_proto::methods::PluginConfigKeyWire {
            key: "mode".into(),
            kind: "enum".into(),
            default: "fast".into(),
            min: None,
            max: None,
            values: vec!["fast".into(), "thorough".into()],
            description: Some(desc_hostile),
            value: "fast".into(),
        },
    ];
    app.extensions = Some(norte_tui::app::ExtensionManager {
        plugins: Vec::new(),
        errors: Vec::new(),
        cursor: 0,
        focus: norte_tui::app::ExtFocus::List,
        config: Some(norte_tui::app::PluginConfigPanel {
            plugin_id: "org.norte.demo".into(),
            plugin_name: "Demo".into(),
            state: PluginConfigState::new(sanitize_config_keys(&wire_keys)),
        }),
    });
    let text = render_80x24(&app);
    assert!(
        !text.contains('\u{202E}'),
        "the description's RTL override painted raw: {text}"
    );
    assert!(
        text.contains('\u{FFFD}'),
        "the hostile description must be masked to U+FFFD: {text}"
    );
    assert!(text.contains("verbose: true"));
    insta::assert_snapshot!(text);
}

/// LOW-3: long items in the navigation popup use MID ellipsis (head + tail,
/// like the path modals), not right truncation: two history entries with a
/// common prefix wider than the popup must render DIFFERENT displays — the
/// tail (the name, what identifies the path to a human) survives.
#[test]
fn popup_long_items_with_mid_ellipsis_stay_distinguishable() {
    let mut app = app_base();
    let prefix = "x".repeat(70); // > 62 cells inside the popup
    app.history[0].push(vp(&format!("file:///{prefix}/uno.txt")));
    app.history[0].push(vp(&format!("file:///{prefix}/dos.txt")));
    app.open_nav_popup(norte_tui::app::NavPopupKind::History);
    let text = render(&app);
    assert!(
        text.contains("uno.txt") && text.contains("dos.txt"),
        "the distinct tails survive the clip (mid ellipsis): {text}"
    );
    assert!(text.contains('…'), "the clip is marked: {text}");
}

#[test]
fn snapshot_modal_collision() {
    let mut app = app_base();
    app.modal = Some(Modal::Collision {
        retry: RetrySpec {
            kind: TransferKind::Copy,
            from: vp("file:///casa/notas.txt"),
            to: vp("file:///otro/notas.txt"),
            opts: TransferOptions::default(),
            name_encoding: None,
        },
    });
    insta::assert_snapshot!(render(&app));
}

/// MINOR-1 (H1 close): `modal_width` measured in `chars`, not terminal
/// cells — a body with CJK (2 cells per char) overflowed the box. A Japanese
/// name in the copy modal's `to` pins the correct fit: the box must fit in
/// the 80-column frame without ratatui clipping the border or the path.
#[test]
fn snapshot_modal_confirm_transfer_cjk() {
    let mut app = app_base();
    app.modal = Some(Modal::ConfirmTransfer {
        kind: TransferKind::Copy,
        items: vec![vp("file:///casa/notas.txt")],
        to: vp("file:///otro/日本語のファイル名.txt"),
        space: None,
        confine: None,
    });
    insta::assert_snapshot!(render(&app));
}

/// **The editable name modal, painted.** It did not have a single picture:
/// it could be reordered entirely, break the field's padding, or leave the
/// box short with the gate green.
///
/// Three things are checked here and in no unit test: that the label goes
/// ABOVE the field, that the field's background reaches the box's edge, and
/// that the body fits in the declared height.
#[test]
fn snapshot_modal_transfer_name() {
    let mut app = app_base();
    app.modal = Some(Modal::TransferName {
        kind: TransferKind::Copy,
        from: vp("file:///casa/Captura de pantalla_20260909_083914.png"),
        to_dir: vp("file:///otro/test1"),
        name: "Captura de pantalla_20260909_083914.png".to_owned(),
        original: b"Captura de pantalla_20260909_083914.png".to_vec(),
        touched: false,
        from_marks: false,
        enc: None,
        error: None,
        space: None,
        confine: None,
    });
    insta::assert_snapshot!(render(&app));
}

/// And with a CJK name wider than the box: the clip is STATED, the cursor
/// survives, and the field's padding does not overflow with double cells.
#[test]
fn snapshot_modal_transfer_name_cjk_long() {
    let mut app = app_base();
    let long = "日本語のファイル名".repeat(6);
    app.modal = Some(Modal::TransferName {
        kind: TransferKind::Move,
        from: vp("file:///casa/x.txt"),
        to_dir: vp("file:///otro"),
        name: long.clone(),
        original: long.into_bytes(),
        touched: true,
        from_marks: false,
        enc: None,
        error: None,
        space: None,
        confine: None,
    });
    insta::assert_snapshot!(render(&app));
}

/// A modal's buttons (spec 2026-09-10, `[ui] dialog_buttons`): the generated
/// key line paints as ` Enter confirm ` ` Esc cancel ` with the `button`
/// role (monochrome: inverted), each button is a zone, and a click on one
/// leaves the synthesized key for `on_key`. Off, the line goes back to being
/// the text hint and there are no zones.
#[test]
fn a_modals_buttons_paint_and_a_click_is_their_key() {
    let mut app = app_base();
    app.dialog_hints = default_dialog_hints();
    app.dialog_hints.buttons = true;
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: false,
    });
    let area = ratatui::layout::Rect::new(0, 0, 80, 16);
    let zones = ui::modal_zones(&app, area);
    assert!(
        zones.iter().any(|z| z.chord == "Enter") && zones.iter().any(|z| z.chord == "Esc"),
        "one button per verb: {zones:?}"
    );
    let enter = zones.iter().find(|z| z.chord == "Enter").expect("Enter");
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let buf = terminal.backend().buffer();
    let row: String = (enter.x0..=enter.x1)
        .map(|x| buf[(x, enter.row)].symbol().to_string())
        .collect();
    assert!(
        row.contains("Enter"),
        "the button paints its chord: {row:?}"
    );
    let button = app.theme.role(norte_theme::Role::Button);
    let cell = buf[(enter.x0, enter.row)].style();
    assert!(
        cell.bg == button.bg && cell.add_modifier.contains(button.add_modifier),
        "the button carries the `button` role: {cell:?} vs {button:?}"
    );
    let hint = render(&app);
    assert!(
        !hint.contains("[Enter]"),
        "with buttons, no brackets: {hint}"
    );

    norte_tui::mouse::after_frame(
        &mut app,
        None,
        norte_tui::mouse::FrameZones {
            modal: zones.clone(),
            ..Default::default()
        },
    );
    let click = crossterm::event::MouseEvent {
        kind: crossterm::event::MouseEventKind::Down(crossterm::event::MouseButton::Left),
        column: enter.x0 + 1,
        row: enter.row,
        modifiers: crossterm::event::KeyModifiers::NONE,
    };
    assert_eq!(
        norte_tui::mouse::handle(&mut app, click),
        norte_tui::mouse::After::SynthKey
    );
    assert_eq!(
        app.pending_key.map(|k| k.code),
        Some(crossterm::event::KeyCode::Enter)
    );

    app.dialog_hints.buttons = false;
    assert!(ui::modal_zones(&app, area).is_empty(), "off, no zones");
    assert!(render(&app).contains("[Enter]"), "off, the text hint");
}

#[test]
fn snapshot_modal_trash_and_permanent() {
    let mut app = app_base();
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: false,
    });
    let trash = render(&app);
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: true,
    });
    let permanent = render(&app);
    insta::assert_snapshot!(format!("{trash}\n===\n{permanent}"));
}

/// H3c: while a help page OPENED FROM the modal covers it, help keeps the
/// keys (`HelpView::over_modal`) and the modal's verbs are INERT. A footer
/// that kept offering them would lie — `y` and `n` would do nothing — so it
/// says what is true: close help to answer.
///
/// What does NOT change: the box and the question stay visible, above help
/// (the modal paints last). Hiding the question is the defect H1 fixed by
/// painting it this way, and this does not undo that.
///
/// The assertions run against the modal's GENERATED hint (`DialogHints`) and
/// not against loose labels: `[Enter] confirm` also shows up in help's OWN
/// footer, where it is indeed live, so searching for the bare label in the
/// frame would confuse two different footers.
#[test]
fn the_modals_footer_does_not_offer_inert_verbs_under_help() {
    // Fixed language BEFORE the first `t()`: the catalogue is resolved once,
    // and `vp` (which is what forces it in the rest of the file) has not run
    // here yet.
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let label = |cmd: &str| norte_i18n::t(&norte_tui::keymap::dialog_hint_id(cmd));
    let notice = norte_i18n::t("modal-hint-help-open");
    let verbs = default_dialog_hints().approval;

    let mut app = app_base();
    app.modal = Some(Modal::ApproveAgentOp {
        req: norte_proto::methods::PolicyApprovalRequired {
            approval_id: 1,
            session: Some("s1".into()),
            op: "copy".into(),
            paths: vec!["mem:///a".into()],
            paths_total: 0,
            ttl_ms: 60_000,
            detail: norte_proto::methods::ApprovalDetail::default(),
        },
    });
    open_help_over_modal(&mut app);
    let tapado = render(&app);

    assert!(
        tapado.contains(&notice),
        "the footer has to say why the modal's keys do not respond:\n{tapado}"
    );
    assert!(
        !tapado.contains(&verbs),
        "the footer still offers the inert verbs ({verbs:?}):\n{tapado}"
    );
    // Not even loose ones: `approve`/`deny` can only be painted by this
    // modal (help's footer lists its own, which do respond).
    for cmd in ["dialog.approve", "dialog.deny"] {
        assert!(
            !tapado.contains(&label(cmd)),
            "{cmd} is inert and the footer still offers it:\n{tapado}"
        );
    }
    // And the question is NOT hidden: the title and the path being approved
    // stay right there, above the page (the modal paints last, H1).
    assert!(
        tapado.contains(&norte_i18n::t("modal-approval-title")),
        "the question has to stay visible:\n{tapado}"
    );
    assert!(
        tapado.contains("mem:///a"),
        "…and the path with it:\n{tapado}"
    );

    // Once help is closed, the modal gets its verbs back: the key does again
    // what the footer says.
    app.help = None;
    let visible = render(&app);
    assert!(
        !visible.contains(&notice),
        "with no help on top there is nothing to close:\n{visible}"
    );
    assert!(
        visible.contains(&verbs),
        "the verbs return to the footer as soon as help closes:\n{visible}"
    );
}

/// The same honest footer in EVERY modal with a generated hint, not just in
/// approval: the lie is identical in a delete confirmation, in a collision,
/// and in an untrusted host key.
///
/// The pair of assertions per modal is what gives it strength: with help
/// closed its generated hint paints WHOLE (otherwise the bottom half would
/// prove nothing), and with help on top not a trace of it remains.
#[test]
fn no_modal_with_a_generated_hint_offers_verbs_under_help() {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    let notice = norte_i18n::t("modal-hint-help-open");
    let hints = default_dialog_hints();
    let modals = [
        (
            Modal::ConfirmDelete {
                items: vec![vp("file:///casa/notas.txt")],
                permanent: true,
            },
            hints.confirm.clone(),
        ),
        (
            Modal::ConfirmTransfer {
                kind: TransferKind::Copy,
                items: vec![vp("file:///casa/notas.txt")],
                to: vp("file:///otro"),
                space: None,
                confine: None,
            },
            hints.confirm.clone(),
        ),
        (Modal::ConfirmQuit, hints.confirm.clone()),
        (
            Modal::Collision {
                retry: RetrySpec {
                    kind: TransferKind::Copy,
                    from: vp("file:///casa/notas.txt"),
                    to: vp("file:///otro/notas.txt"),
                    opts: TransferOptions::default(),
                    name_encoding: None,
                },
            },
            hints.collision.clone(),
        ),
        (
            Modal::TrustHostKey {
                host: "ejemplo.org".into(),
                port: Some(22),
                algo: "ssh-ed25519".into(),
                fingerprint: "SHA256:abc".into(),
                dir: vp("sftp://ejemplo.org/casa"),
                pane: 0,
                trail: Trail::Record,
            },
            hints.trust_host.clone(),
        ),
    ];
    for (modal, verbs) in modals {
        let mut app = app_base();
        app.modal = Some(modal);

        // Without help: the generated footer paints whole.
        let solo = render(&app);
        assert!(
            solo.contains(&verbs),
            "this modal does not paint its hint whole, so the other half of \
             the test would prove nothing ({verbs:?}):\n{solo}"
        );

        // With help on top: not one verb, and the notice in its place.
        open_help_over_modal(&mut app);
        let tapado = render(&app);
        assert!(
            tapado.contains(&notice),
            "this modal does not say why its keys do not respond:\n{tapado}"
        );
        assert!(
            !tapado.contains(&verbs),
            "this modal still offers inert verbs ({verbs:?}):\n{tapado}"
        );
    }
}

/// TOFU Lua (M4, ADR 0026): the exact shape of the trust modal for
/// `./.norte/init.lua` is frozen — sanitized path + abbreviated sha256 +
/// notice that it runs with the user's permissions.
#[test]
fn snapshot_modal_trust_lua_init() {
    let mut app = app_base();
    app.modal = Some(Modal::TrustLuaInit {
        path: "repo/.norte/init.lua".into(),
        // 32 hex (128 bits) as main.rs produces it — the modal must fit it.
        hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
    });
    insta::assert_snapshot!(render(&app));
}

/// **The message that WRAPS fits whole in its box.**
///
/// It is the only modal whose height is not derived from the body: its body
/// is a single line that `ratatui` splits into several, and counting those
/// rows requires its wrapping rule (`Paragraph::line_count` knows it, but it
/// is an unstable feature). So that height is a hand-written number — and a
/// hand-written number silently falls short as soon as the message grows: a
/// longer translation, a deeper path. This is what turns it red.
///
/// Checked against the PAINTED text and not against the model: what matters
/// is that the warning's last word reaches the screen, and only the buffer
/// says that.
#[test]
fn the_message_that_wraps_fits_in_its_box() {
    let mut app = app_base();
    app.modal = Some(Modal::TrustLuaInit {
        path: "repo/.norte/init.lua".into(),
        hash_abbrev: "ab12cd34ef56ab78ab12cd34ef56ab78".into(),
    });
    let painted = render(&app);
    // The body carries the KEYS inside, at the end: they are the last thing
    // read before granting execution permissions, so they are the first
    // thing lost if the height falls short — and `ratatui` clips at the
    // bottom without saying so.
    let body = norte_i18n::ta(
        "modal-lua-trust-body",
        &[("path", "repo/.norte/init.lua"), ("hash", "x")],
    );
    let ultima = body
        .split_whitespace()
        .last()
        .expect("the body is not empty");
    assert!(
        painted.contains(ultima),
        "the end of the warning did not reach the screen ({ultima:?}): {painted}"
    );
}

/// TOFU (#45): the modal shows the fingerprint for comparison, and a HOSTILE
/// host (bidi override) from the remote server gets MASKED — it never paints
/// the raw byte that could spoof the bar. Not a snapshot: direct asserts.
#[test]
fn modal_trust_host_shows_fingerprint_and_masks_hostile_host() {
    let mut app = app_base();
    app.modal = Some(Modal::TrustHostKey {
        host: "evil\u{202E}host".into(),
        port: Some(22),
        algo: "ssh-ed25519".into(),
        fingerprint: "SHA256:abc123XYZ".into(),
        dir: vp("sftp://evilhost/"),
        pane: 0,
        trail: Trail::Record,
    });
    let text = render(&app);
    assert!(
        text.contains("SHA256:abc123XYZ"),
        "the fingerprint is shown for comparison: {text}"
    );
    assert!(
        !text.contains('\u{202E}'),
        "the host's bidi override does NOT reach the render: {text:?}"
    );
    assert!(
        text.contains("ssh-ed25519"),
        "the algorithm is shown: {text}"
    );
    assert!(
        text.contains('!'),
        "the hostile host carries the badge that WARNS the user: {text}"
    );
}

/// A hostile fingerprint (the server tries to hide chars) gets masked AND
/// carries a badge: the user sees the fingerprint was tampered with, not
/// approve it blindly. And a canonical SHA256 (50 chars) fits whole WITHOUT
/// ellipsis: what is shown == what is trusted.
#[test]
fn modal_trust_host_hostile_fingerprint_and_full_sha256() {
    let mut app = app_base();
    app.modal = Some(Modal::TrustHostKey {
        host: "h".into(),
        port: None,
        algo: "ssh-ed25519".into(),
        fingerprint: "SHA256:sp\u{202E}oof".into(),
        dir: vp("sftp://h/"),
        pane: 0,
        trail: Trail::Record,
    });
    let text = render(&app);
    assert!(
        !text.contains('\u{202E}'),
        "the fingerprint's bidi does NOT reach the render: {text:?}"
    );
    assert!(text.contains('!'), "tampered fingerprint → badge: {text}");

    // A real SHA256 (7 + 43 = 50 chars) fits whole, without truncation.
    app.modal = Some(Modal::TrustHostKey {
        host: "h".into(),
        port: None,
        algo: "ssh-ed25519".into(),
        fingerprint: "SHA256:oXf6dQ7pC3vN2mK9tR1sB4jW8yZ0aL5eH6gU3iO7wA".into(),
        dir: vp("sftp://h/"),
        pane: 0,
        trail: Trail::Record,
    });
    let text = render(&app);
    assert!(
        text.contains("SHA256:oXf6dQ7pC3vN2mK9tR1sB4jW8yZ0aL5eH6gU3iO7wA"),
        "the canonical SHA256 is shown IN FULL (no ellipsis): {text}"
    );
    assert!(!text.contains('…'), "it is not truncated: {text}");
}

/// **The viewer SAYS there is more, above and to the right.**
///
/// Before, only the status bar's `1/4813` count said so, and widthwise
/// nothing said it: the viewer does not wrap, so a file clipped on the right
/// read like a short file. Both bars paint over the frame's edges, and
/// neither when everything fits — a bar full from edge to edge informs of
/// nothing.
#[test]
fn the_viewer_paints_the_bars_only_when_there_is_more() {
    let mut app = app_base();

    // A short one-line file: fits whole, so not even one bar.
    app.viewer = Some(Viewer::new(
        vp("file:///casa/corto.txt"),
        b"hola\n".to_vec(),
        false,
    ));
    let fits = render(&app);
    assert!(!fits.contains('█'), "it all fits: no bar to drag\n{fits}");

    // Tall: forty lines on a sixteen-row screen.
    let alto: Vec<u8> = (0..40)
        .flat_map(|i| format!("linea {i}\n").into_bytes())
        .collect();
    app.viewer = Some(Viewer::new(vp("file:///casa/alto.txt"), alto, false));
    assert!(
        render(&app).contains('█'),
        "there is more BELOW and it shows"
    );

    // Wide: a two-hundred-column line on an eighty-column screen.
    let width = || format!("{}\n", "x".repeat(200)).into_bytes();
    let v = Viewer::new(vp("file:///casa/ancho.txt"), width(), false);
    assert_eq!(v.max_cols(), 200);
    app.viewer = Some(v);
    let painted = render(&app);
    assert!(
        painted.contains('█'),
        "there is more to the RIGHT and it shows\n{painted}"
    );

    // And the thumb MOVES with the scroll: a still bar says "there is more"
    // and does not say where you are.
    let mut v = Viewer::new(vp("file:///casa/ancho.txt"), width(), false);
    v.scroll_right(150);
    app.viewer = Some(v);
    assert_ne!(
        painted,
        render(&app),
        "the horizontal thumb follows the scroll"
    );
}

#[test]
fn snapshot_viewer_text_and_hex() {
    let mut app = app_base();
    app.viewer = Some(Viewer::new(
        vp("file:///casa/notas.txt"),
        b"a\xF1o 2026\nsegunda l\xEDnea\n".to_vec(),
        false,
    ));
    let text = render(&app);
    let mut v = Viewer::new(
        vp("file:///casa/logo.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec(),
        false,
    );
    v.scroll_down(0);
    app.viewer = Some(v);
    // Finding 3 (branch review, phase 5): `panels::draw_viewer` reads
    // `App::viewer_modo` (set on OPEN), not a live recalculation — this test
    // builds `App` by hand, so it sets the mode that `open_viewer` would have
    // left under the default `chrome`: `Auto` with no terminal probe (there
    // is no tty in a test) is `Modo::Blocks`.
    app.viewer_modo = norte_tui::viewer_open::Modo::Blocks;
    let hex = render(&app);
    insta::assert_snapshot!(format!("{text}\n===\n{hex}"));
}

/// Opens the help overlay (H3b) exactly as the binary does: the synthetic
/// cheatsheet for the `keys` entry and the chord resolver come from the SAME
/// builders (`norte_tui::help::build` and `TuiChords::new`, not a copy of the
/// format) over the real orthodox preset, and BOTH in the language of the
/// rest of the UI (`vp` forces ES above). The resolver too, not just the
/// corpus: it is what puts the label on each executable row
/// (`ChordResolver::label`), and `App`'s default resolves the ENVIRONMENT's
/// language — with that, a Spanish reader would read Spanish prose with the
/// rows labeled in English.
fn open_help(app: &mut App) {
    let presets = norte_tui::keymap::presets();
    let (_, preset) = presets.iter().find(|(n, _)| *n == "orthodox").unwrap();
    let build = |screen| {
        norte_tui::keymap::Effective::build_for(preset, &[], norte_tui::keymap::COMMANDS, screen)
            .unwrap()
    };
    // #113: the dialogs section comes from the effective `dialog`, whose
    // vocabulary merges COMMANDS and DIALOG_COMMANDS (like `build_keymaps`).
    let dialog_known: Vec<&str> = norte_tui::keymap::COMMANDS
        .iter()
        .copied()
        .chain(norte_tui::keymap::DIALOG_COMMANDS.iter().copied())
        .collect();
    let dialog = norte_tui::keymap::Effective::build_for(
        preset,
        &[],
        &dialog_known,
        norte_tui::keymap::Screen::Dialog,
    )
    .unwrap();
    let browse = build(norte_tui::keymap::Screen::Browse);
    let viewer = build(norte_tui::keymap::Screen::Viewer);
    let lines = norte_tui::help::build(&browse, &viewer, &dialog);
    app.help_chords = std::sync::Arc::new(norte_tui::help::TuiChords::new(
        &browse,
        &viewer,
        &dialog,
        norte_i18n::Lang::Es,
    ));
    // H3d: and the context facts freeze the same as in the binary
    // (`open_contextual_help`), so these snapshots record what a reader sees
    // FROM `app_base` — with `file:///home` writable, nothing dimmed by the
    // backend, and the `nav.enter`/`pane.view` rows decided by what is under
    // the cursor.
    app.freeze_help_facts();
    app.help = Some(norte_tui::app::HelpView::new(norte_i18n::Lang::Es, lines));
    refresh_help(app);
}

/// The same help, but opened ON TOP of a modal (H3c, `over_modal`): the one
/// that keeps the keys, which leaves the modal's verbs inert until it
/// closes.
fn open_help_over_modal(app: &mut App) {
    open_help(app);
    app.help.as_mut().expect("help opened").over_modal = true;
    refresh_help(app);
}

/// H3b: the overlay is laid out for the frame it is about to be painted on
/// (the run loop does this every turn), and the geometry comes from the SAME
/// function the painter uses. 80×16 is [`render`]'s frame.
fn refresh_help(app: &mut App) {
    refresh_help_en(app, 80, 16);
}

/// [`refresh_help`] over a `w`×`h` frame: the overlay's geometry no longer
/// depends only on the frame — the sidebar is sized to the open language's
/// corpus titles — so the language comes from the model itself, as in the
/// run loop.
fn refresh_help_en(app: &mut App, w: u16, h: u16) {
    let Some(lang) = app.help.as_ref().map(|v| v.state.lang()) else {
        return;
    };
    let (width, height) = ui::help_body_size(ratatui::layout::Rect::new(0, 0, w, h), lang);
    app.refresh_help(width, height);
}

/// Like [`render`], but returns the BUFFER: the text dump carries no styles,
/// so a highlight can only be checked cell by cell.
fn render_buffer(app: &App) -> ratatui::buffer::Buffer {
    let mut terminal = Terminal::new(TestBackend::new(80, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().buffer().clone()
}

#[test]
fn snapshot_help() {
    let mut app = app_base();
    open_help(&mut app);
    // Top: the corpus index, where the overlay opens.
    let up = render(&app);
    // Bottom: the synthetic keyboard page — the usual cheatsheet, now one
    // more sidebar entry.
    app.help
        .as_mut()
        .unwrap()
        .state
        .open(&norte_help::TopicId::new(norte_frontend::help::KEYS_ID));
    refresh_help(&mut app);
    insta::assert_snapshot!(format!("{up}\n===\n{}", render(&app)));
}

/// H3b, ADAPTIVE footer: help's footer offers its FIVE printable verbs and
/// lets the width decide how many get painted (`ui::fit_hint_groups` drops
/// WHOLE groups from the tail and marks the loss with `…`).
///
/// It used to ALWAYS exclude `[enter]`/`[esc]`, to honor an 80-wide frame: on
/// a 113-column terminal the footer stayed half empty with the two universal
/// keys hidden for no reason. The fixed exclusion now yields to the
/// mechanism that already existed.
///
/// What is checked is the invariant, not a specific width: at 113 all five
/// fit (and with no `…`, because nothing was lost); at 80 the ones at the
/// top of the ranking survive — the ones the reader CANNOT guess — and the
/// `…` says there was clipping. At no width does half a group appear.
///
/// Note: the verb labels asserted below (`down`, `up`, …) are the real
/// Spanish Fluent catalogue text — `vp` forces `Lang::Es` for this file, so
/// this checks actual rendered UI output, not test prose.
#[test]
fn the_help_footer_adapts_to_the_width() {
    let pie_a = |w: u16, h: u16| {
        let mut app = app_base();
        open_help(&mut app);
        refresh_help_en(&mut app, w, h);
        let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
        terminal.draw(|f| ui::draw(f, &app)).expect("draw");
        terminal
            .backend()
            .to_string()
            .lines()
            .nth(help_footer_row(w, h))
            .expect("the footer falls inside the frame")
            .to_owned()
    };

    let width = pie_a(124, 16);
    for verb in [
        "índice ↔ texto",
        "bajar",
        "subir",
        "filtrar",
        "atrás",
        "confirmar",
        "cancelar",
    ] {
        assert!(
            width.contains(verb),
            "a wide frame fits all seven groups, and `{verb}` is missing: {width:?}"
        );
    }
    assert!(
        !width.contains('…'),
        "…and no loss marker, because nothing was lost: {width:?}"
    );

    let narrow = pie_a(80, 16);
    // The ones that survive are the TOP of the ranking: how you switch to
    // the text and how you scroll down it, which is what nobody guesses on a
    // screen unlike any other in the program.
    for verb in ["índice ↔ texto", "bajar"] {
        assert!(
            narrow.contains(verb),
            "at 80 columns the verbs the reader cannot guess survive, and \
             `{verb}` is missing: {narrow:?}"
        );
    }
    assert!(
        narrow.contains('…'),
        "and the clip is MARKED — a silently clipped footer lies: {narrow:?}"
    );
    // Whole group or nothing: no bracket is left orphaned.
    for pie in [&width, &narrow] {
        assert_eq!(
            pie.matches('[').count(),
            pie.matches(']').count(),
            "half a `[chord] label` group in the footer: {pie:?}"
        );
    }
}

/// The frame row where the help overlay's FOOTER falls, for a frame of
/// `w`×`h`.
///
/// Traces `ui::help_layout`'s arithmetic, which is private: the box is
/// centered with two rows of vertical margin, its border eats one row, and
/// the footer is the LAST interior row — right below the body, whose height
/// `ui::help_body_size` does publish. Needed so the assertion that the
/// filter paints points at the footer and not the whole frame: see
/// [`snapshot_help_hostile_filter`].
///
/// The VERTICAL cut did not change when sizing the sidebar by content: the
/// language that `help_body_size` now asks for decides only the WIDTH split
/// and nothing else, so any one will do here.
fn help_footer_row(w: u16, h: u16) -> usize {
    let box_height = h.saturating_sub(2).max(6).min(h);
    let top = (h - box_height) / 2;
    let (_, body_height) =
        ui::help_body_size(ratatui::layout::Rect::new(0, 0, w, h), norte_i18n::Lang::Es);
    usize::from(top + 1) + body_height
}

/// H3b, H1's lesson over a new PAINTED surface: help's filter matches the
/// RAW bytes on purpose (`filter_raw` — a needle with a bidi override has to
/// find the topic the reader sees on screen), and the only thing that can be
/// painted is `filter_display`. The overlay's footer is the echo of that
/// typed text: a `U+202E` that reaches it visually reorders the entire line.
///
/// The footer is NOT the draw's only free-form input, and saying so was
/// false (MEDIUM review): the sidebar paints corpus titles and the body
/// paints headings, table cells and code blocks, all UNMASKED on purpose —
/// they are the binary's own text — but with no net beyond the corpus's
/// charset gate (`no_shipped_topic_carries_a_terminal_hazard`, in
/// `norte-help`) and the Fluent catalogue. What IS true of the footer is
/// that it is the only TYPED input, and that is why it is the only one
/// masked in the painter.
///
/// The needle starts with the canonical `rlo` fixture's token
/// (`norte_testkit::corpus::hostile_chords`) — it arrives by paste as easily
/// as by hand — and with it in front no topic matches: the sidebar stays
/// empty and the body keeps showing what was being read, which is exactly
/// the model's contract. What is checked here is the FRAME, not the model
/// (which has its own test in `norte_frontend::help`): a plain snapshot
/// would happily record the hazard.
///
/// The sweep is PER LINE and not over the whole `to_string()`: the backend
/// joins rows with `\n`, which `is_terminal_hazard` (correctly) also flags
/// as control — a blind check over the whole buffer would give a false
/// positive from the dump's own formatting. See the same comment in
/// `snapshot_extensions_description_hostile_80x24`.
///
/// The anti-emptiness assertion is scoped to the FOOTER, not the frame
/// (MEDIUM review): `app_base` seeds a `\xE9.dat` entry that paints with its
/// own `U+FFFD` in the pane behind it, so a `text.contains('\u{FFFD}')` over
/// the whole frame would pass even if the footer painted absolutely
/// nothing. Today the overlay covers that row at 80×16 and it makes no
/// difference; the layout changed in this very phase, so "makes no
/// difference today" is not something to lean on.
/// H3e: a HOSTILE plugin's page, composed — sidebar and body at once.
///
/// `help_render`'s unit tests check the STRINGS (that the title gets
/// masked, that the badge appears); this checks the LAYOUT, which is where
/// the bug the security review found lived: the badge turned itself off
/// when the plugin declared no publisher, and the page ended up with the
/// exact shape — title, rule, body — of one from the manual. A snapshot
/// records that shape; a string test does not.
///
/// The plugin is the worst thing that can be sent over the wire and still be
/// legal: `name` with an RTL override (`rtl_override` corpus), an EMPTY
/// `publisher` — the manifest requires the field but not that it have
/// content — and a page that passes itself off as the app's own
/// documentation.
#[test]
fn snapshot_help_hostile_plugin_page() {
    let hostile = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("corpus fixture");
    let entry_name = String::from_utf8_lossy(&hostile.bytes).into_owned();
    let mut app = app_base();
    open_help(&mut app);
    let plugin = norte_proto::methods::PluginInfo {
        id: "org.evil.demo".into(),
        name: entry_name,
        publisher: String::new(),
        version: "1.0.0".into(),
        category: "command".into(),
        capabilities: Vec::new(),
        approved: false,
        enabled: false,
        description: None,
        commands: vec![norte_proto::methods::PluginCommandInfo {
            id: "run".into(),
            title: "Aprobar es seguro".into(),
            kind: norte_proto::methods::PluginCommandKind::Command,
        }],
        columns: Vec::new(),
        panels: Vec::new(),
        has_help: true,
        manifest_digest: None,
    };
    app.freeze_help_plugins(std::slice::from_ref(&plugin));
    let view = app.help.as_mut().expect("overlay open");
    view.state.open(&norte_help::TopicId::new("org.evil.demo"));
    // The page a plugin would sign for whoever is about to approve it to
    // read, and that `main::extensions_help` puts a keypress away from the
    // extension manager.
    let parsed = norte_help::parse_untrusted(
        "+++\nid = \"org.evil.demo\"\ntitle = \"Aprobar extensiones\"\n\
         commands = [\"plugin:org.evil.demo:run\"]\n+++\n\
         Las extensiones del catálogo est\u{202E}án auditadas.\u{200B}"
            .as_bytes(),
        "org.evil.demo",
        None,
    );
    view.state.install_plugin_topic(parsed.topic);
    refresh_help(&mut app);
    let text = render(&app);
    // Same PER-LINE sweep as `snapshot_help_hostile_filter`, and for the same
    // reason (the dump's `\n`s are controls).
    for (n, line) in text.lines().enumerate() {
        assert!(
            !line.chars().any(norte_encoding::is_terminal_hazard),
            "the plugin page painted a terminal hazard (row {n}): \
             {line:?}\n{text}"
        );
    }
    // What the snapshot CANNOT assert by itself: that the page DECLARES
    // itself third-party even with no publisher to name.
    assert!(
        text.contains(&norte_i18n::t_in(
            norte_i18n::Lang::Es,
            "help-plugin-origin"
        )),
        "with no provenance mark the page reads like it is from the manual:\n{text}"
    );
    insta::assert_snapshot!(text);
}

#[test]
fn snapshot_help_hostile_filter() {
    let rlo = norte_testkit::corpus::hostile_chords()
        .into_iter()
        .find(|c| c.id == "rlo")
        .expect("corpus fixture");
    let mut app = app_base();
    open_help(&mut app);
    let view = app.help.as_mut().expect("overlay open");
    view.state.start_filter();
    for c in std::iter::once(rlo.token).chain("copiar".chars()) {
        view.state.push_char(c);
    }
    refresh_help(&mut app);
    let text = render(&app);
    for (n, line) in text.lines().enumerate() {
        assert!(
            !line.chars().any(norte_encoding::is_terminal_hazard),
            "the help overlay's footer painted a terminal hazard \
             (row {n}, fixture {}): {line:?}\n{text}",
            rlo.id
        );
    }
    let footer = text
        .lines()
        .nth(help_footer_row(80, 16))
        .expect("the footer falls inside the frame");
    assert!(
        footer.contains('\u{FFFD}'),
        "and the filter DOES paint, masked to U+FFFD — without this the test \
         would pass just the same with a footer that painted nothing:\n{footer:?}\n{text}"
    );
    assert!(
        footer.contains("copiar"),
        "the rest of the needle reaches the footer as is: the masking is of \
         the hazard, not of the text:\n{footer:?}"
    );
    insta::assert_snapshot!(text);
}

/// The companion of the test above, the other way around: a HOSTILE title
/// that reaches the SIDEBAR.
///
/// The one above only exercises the footer, and on top of that with a needle
/// that matches nothing: the sidebar comes out empty and no hostile string
/// ever travels the title's path. This one travels it, with the canonical
/// fixture for the LEGITIMATE case — `bidi_isolate_url` from
/// `norte_testkit::corpus::hostile_titles`, which is `U+2066`…`U+2069`
/// around an `sftp://`, the CORRECT way to put an LTR stretch in RTL prose
/// and at the same time four terminal hazards in a row.
///
/// The entry point is real: the synthetic `keys` entry's label is resolved
/// by the frontend (`t("help-topic-keys")`) and travels to the model as a
/// row title, exactly like a corpus title or — H3f onward — a plugin
/// manifest's.
///
/// What is asserted is what really happens, not what one would want: the
/// sidebar's painter does NOT mask, so the title comes out VERBATIM (up to
/// the right-hand clip) and the bidi isolates reach the terminal. It is not
/// a bug in the painter — the text is trusted by construction — but it IS
/// the reason the corpus's charset gate is LOAD-BEARING and not an extra
/// belt: remove it and this is an injection one `.md` away.
#[test]
fn a_hostile_help_title_reaches_the_sidebar_raw() {
    let bidi = norte_testkit::corpus::hostile_titles()
        .into_iter()
        .find(|t| t.id == "bidi_isolate_url")
        .expect("corpus fixture");
    let mut app = app_base();
    open_help(&mut app);
    // Same constructor `HelpView::new` uses; the only thing that changes is
    // the label, which here is the fixture instead of the Fluent catalogue.
    let view = app.help.as_mut().expect("overlay open");
    view.state = norte_frontend::help::HelpState::new(norte_i18n::Lang::Es, bidi.text.to_owned());
    // The synthetic entry is the LAST one in the sidebar and does not fit at
    // 80×16: it is filtered by its id to leave it alone, which is also the
    // path by which the model matches a title (`filter_raw` against the raw
    // bytes).
    view.state.start_filter();
    for c in norte_frontend::help::KEYS_ID.chars() {
        view.state.push_char(c);
    }
    refresh_help(&mut app);
    let text = render(&app);

    let row = text
        .lines()
        .find(|l| l.contains('\u{2066}'))
        .unwrap_or_else(|| {
            panic!(
                "the synthetic `keys` row's title did not reach the sidebar: \
                 the path this test exists to exercise was not exercised\n{text}"
            )
        });
    // Verbatim up to the clip: the title's prefix, isolate included, comes
    // out as is. `right_ellipsis` cuts on the RIGHT, so the head survives
    // whole.
    let head: String = bidi.text.chars().take(10).collect();
    assert!(
        row.contains(&head),
        "the sidebar paints the title VERBATIM (clipped on the right): \
         {row:?} should start with {head:?}"
    );
    assert!(
        row.chars().any(norte_encoding::is_terminal_hazard),
        "…and unmasked: if this turns red it means someone added a filter to \
         the sidebar's painter, which is FINE — update this test and \
         `draw_help`'s note at the same time: {row:?}"
    );
}

/// Like [`render`] but over a `w`×`h` frame, laying out help for THAT frame:
/// the overlay's geometry depends on both dimensions and the pre-render has
/// to measure the same thing the painter does.
fn render_help(app: &mut App, w: u16, h: u16) -> String {
    refresh_help_en(app, w, h);
    let mut terminal = Terminal::new(TestBackend::new(w, h)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    terminal.backend().to_string()
}

/// The sidebar sizes itself to ITS CONTENT, between two caps.
///
/// It used to be a constant 24 cells and at 113 columns it clipped five of
/// the nine rows of the Spanish corpus — with 90 cells of prose next to it,
/// which is wider than reads at a glance. Now it asks for what its rows
/// measure (indent included, in CELLS) and stays between the historical
/// floor and a long third of the frame.
///
/// The two extremes fixed here are the two that can break separately: that
/// content RULES (a corpus with longer titles widens the sidebar) and that
/// the cap HOLDS (on a narrow frame it does not eat everything).
#[test]
fn the_help_sidebar_sizes_to_its_titles() {
    use norte_i18n::Lang;
    use ratatui::layout::Rect;
    use unicode_width::UnicodeWidthStr;

    // What the corpus rows measure: the two-cell indent plus the widest
    // title, or the widest group header if it were to win.
    let width_requested = |lang| {
        norte_help::topics(lang)
            .iter()
            .map(|t| 2 + t.title.width())
            .max()
            .expect("the corpus carries topics")
    };
    let es = width_requested(Lang::Es);
    let en = width_requested(Lang::En);
    assert_ne!(
        es, en,
        "both corpora measure the same ({es}); with equally wide titles, a \
         CONSTANT-width sidebar would pass what follows and this test would \
         not tell 'content rules' apart from 'always the same'. Change a \
         topic's title or compare against a different pair."
    );

    // WIDE frame: content rules, and two different corpora give two
    // different widths. A constant would pass what follows and fail here.
    let width = Rect::new(0, 0, 160, 40);
    assert_eq!(
        usize::from(ui::help_sidebar_width(width, Lang::Es)),
        es,
        "with room to spare the sidebar asks for exactly what its widest row \
         measures"
    );
    assert_eq!(usize::from(ui::help_sidebar_width(width, Lang::En)), en);

    // NARROW frame: the 35% frame cap wins, and the historical 24-cell floor
    // is respected — neither a sidebar that eats the prose nor one narrower
    // than it used to be.
    for w in [80u16, 100, 113] {
        let lateral = ui::help_sidebar_width(Rect::new(0, 0, w, 40), Lang::Es);
        assert!(
            lateral >= 24,
            "at {w} columns the sidebar shrank below its old fixed width \
             ({lateral})"
        );
        assert!(
            u32::from(lateral) * 100 <= u32::from(w) * 35,
            "at {w} columns the sidebar goes past 35% of the frame ({lateral})"
        );
    }
    // …and the cap is what BITES at 80 columns: the sidebar asked for 39.
    assert_eq!(
        ui::help_sidebar_width(Rect::new(0, 0, 80, 40), Lang::Es),
        28
    );

    // And the body has a typographic measure: the prose does not grow with
    // the terminal beyond what reads at a glance. Two cells less than the
    // measure: the body's last column is its scroll bar, and the one before
    // it the margin separating the prose from it.
    let (body, _) = ui::help_body_size(Rect::new(0, 0, 200, 40), Lang::Es);
    assert_eq!(
        body, 70,
        "the prose is cut at its own measure, not at the edge"
    );
}

/// …and with room, NO title comes out clipped.
///
/// The check above is arithmetic; this one is over the painted frame, which
/// is where you see whether the indent, the gutter or the ellipsis ate one
/// cell too many.
///
/// ONLY the sidebar column is checked. It used to search for the title in
/// any line of the frame, and passed by accident: at 120 columns the sidebar
/// was already clipping the longest title ("Lo que la pantalla enseña
/// alrededor del listado", 49 cells with the indent, against a 35% ceiling
/// of 42), but the BODY of the index page painted it whole in its link
/// list. Adding a topic (spec 2026-09-15) pushed that line past the 36 rows
/// and exposed the clip. Genuine room is 140 columns: 35% = 49.
#[test]
fn with_room_no_help_title_comes_out_clipped() {
    let mut app = app_base();
    open_help(&mut app);
    let text = render_help(&mut app, 140, 40);
    let lateral = usize::from(ui::help_sidebar_width(
        ratatui::layout::Rect::new(0, 0, 140, 40),
        norte_help::Lang::Es,
    ));
    // What precedes the sidebar on each line: the frame's border, the box's
    // border and, if the dump quotes the line, the quote mark.
    let column = |l: &str| l.chars().take(lateral + 4).collect::<String>();
    for theme in norte_help::topics(norte_i18n::Lang::Es) {
        assert!(
            text.lines().any(|l| column(l).contains(&theme.title)),
            "title {:?} does not appear whole in the sidebar:\n{text}",
            theme.title
        );
    }
    assert!(
        !text
            .lines()
            .any(|l| l.contains("…") && l.contains("  SFTP")),
        "…and no ellipsis on the longest row:\n{text}"
    );
}

/// The footer says WHERE the reader is, in percent READ down to the
/// window's foot (`17 %`, like `less`), and stays silent when the page fits
/// whole. An `11/663` in lines told nobody how much was left.
///
/// It is not decoration: a topic's executable rows paint BEHIND all of its
/// prose, so on a long page they do not make it into the first render, and
/// without the indicator nothing says they are there.
#[test]
fn the_help_footer_places_the_reader_only_when_needed() {
    let mut app = app_base();
    open_help(&mut app);

    // 80×16: the index does not come close to fitting in the body's 12 rows.
    let text = render_help(&mut app, 80, 16);
    let view = app.help.as_ref().expect("overlay open");
    let total = view.body().0.len();
    let (_, height) =
        ui::help_body_size(ratatui::layout::Rect::new(0, 0, 80, 16), view.state.lang());
    assert!(
        total > height,
        "the index does not fit in {height} rows ({total})"
    );
    let footer = text
        .lines()
        .nth(help_footer_row(80, 16))
        .expect("the footer falls inside the frame");
    // Stuck to the box's right edge: the backend's dump quotes each row, so
    // the anchor is the box's `│` and not the line's end.
    let pct = |scroll: usize| (scroll + height).min(total) * 100 / total;
    assert!(
        footer.contains(&format!("{} % │", pct(0))),
        "the footer places the reader on the first screen, on the RIGHT: {footer:?}"
    );

    // And it follows the scroll. Focus enters the body so `page_down` scrolls
    // it (with focus on the sidebar it moves the topic cursor), which along
    // the way makes `refresh` REVEAL the focused action: it does not matter
    // how much the body moved — what is checked is that the footer says the
    // line that is really at the top, not that five moved.
    app.help.as_mut().expect("overlay").state.toggle_focus();
    app.help.as_mut().expect("overlay").state.page_down(5);
    let text = render_help(&mut app, 80, 16);
    let scroll = app.help.as_ref().expect("overlay").state.body_scroll();
    assert!(scroll > 0, "the body scrolled");
    let footer = text
        .lines()
        .nth(help_footer_row(80, 16))
        .expect("the footer falls inside the frame");
    assert!(
        footer.contains(&format!("{} % │", pct(scroll))),
        "the indicator follows the scroll ({scroll}): {footer:?}"
    );

    // Frame with room to spare: the page fits whole and the indicator is
    // SUPERFLUOUS — a `1/9` over nine visible lines informs of nothing. The
    // height is generous on purpose: the index GROWS with every page H3h
    // writes, and a tight frame would turn "write a page" into "fix this
    // test". (And the prose's links, which since bridge 75 are rows you can
    // follow: the index links to every page.)
    let text = render_help(&mut app, 120, 110);
    let view = app.help.as_ref().expect("overlay open");
    let total = view.body().0.len();
    let (_, height) = ui::help_body_size(
        ratatui::layout::Rect::new(0, 0, 120, 110),
        view.state.lang(),
    );
    assert!(total <= height, "the page fits in {height} rows ({total})");
    let footer = text
        .lines()
        .nth(help_footer_row(120, 110))
        .expect("the footer falls inside the frame");
    assert!(
        !footer.contains(" % "),
        "with the whole page in view the footer says nothing: {footer:?}"
    );
}

/// Text of each buffer row, one entry per frame row.
fn row_texts(buf: &ratatui::buffer::Buffer) -> Vec<String> {
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].symbol())
                .collect()
        })
        .collect()
}

/// Styles of each buffer row, cell by cell.
fn all_row_styles(buf: &ratatui::buffer::Buffer) -> Vec<Vec<ratatui::style::Style>> {
    (buf.area.top()..buf.area.bottom())
        .map(|y| {
            (buf.area.left()..buf.area.right())
                .map(|x| buf[(x, y)].style())
                .collect()
        })
        .collect()
}

/// H3b: the body WITH FOCUS. `copying` carries five commands and three
/// links, and its rows paint at the END of the page, behind all the prose:
/// moving focus to the body and taking a step requires the active row to be
/// both HIGHLIGHTED and VISIBLE at once. That is what this test checks, and
/// the reason `HelpView::refresh` calls `reveal` — without it the highlight
/// would live outside the window and the reader would move a cursor they
/// cannot see.
///
/// The snapshot freezes the clip (which lines stayed inside); the highlight
/// CANNOT come out of it — the backend's dump is bare text, with no styles —
/// so it is checked separately, cell by cell.
///
/// The assertion is POSITIVE, and that is the half that was missing (MAJOR
/// review). All of focus's machinery rests on an invariant that crosses two
/// crates: action *i* of `HelpState::actions` paints on line
/// `Rendered::action_lines[i]`. The two halves are pinned separately
/// (`help_render.rs` sweeps the map over the whole corpus, the model has its
/// own tests), but the place where they MEET is this one, and here only TWO
/// rows were compared by inequality: shift the map by one position and `f5`
/// gets highlighted while the model says `f6` — the vectors stay different,
/// the test still passes, and the user sees a row under the cursor that says
/// `pane.copy` while `Enter` dispatches `pane.move`. A lying label over a
/// surface that MUTATES files.
///
/// The highlighted row is located without naming it: the SAME frame is
/// painted with and without focus in the body and the styles are diffed row
/// by row. The only one that changes is the one carrying the highlight, and
/// of it we assert that its TEXT names the command the model says has focus
/// — with the chord and the label the resolver itself gives, not a copy of
/// the format. Comparing against the theme's specific style would tie the
/// test to the palette.
/// H3d end to end, over the FRAME: with both panes inside a zip
/// (`READ_ONLY` by the scheme's construction, ADR 0018), the copying page's
/// rows that WRITE come out with their reason next to them, not just dimmed.
///
/// It is the only thing that ties the whole chain — `freeze_help_facts` →
/// the shared table → `row_line` — to what is seen: all three legs have
/// their own unit test, and none would break if the freeze stopped being
/// called on open.
///
/// The body is walked with focus (like `snapshot_help_body_with_focus`)
/// because the executable rows go AFTER the prose: without scrolling, the
/// reason exists and is not in the frame. The frame is 100×30 — a real
/// terminal, not one generously sized to fit — and the reason comes out
/// WHOLE because `row_line` budgets for it before the label: what yields is
/// the command's name, which is already in the prose above and in the
/// chord's column.
#[test]
fn help_inside_a_zip_paints_the_veto_reason() {
    let inside = vp("zip+file:///a.zip/!");
    let mut app = App::new(
        Pane::new(
            inside.clone(),
            vec![entry(&inside, b"leeme.txt", EntryKind::File, Some(3))],
        ),
        Pane::new(inside, Vec::new()),
    );
    app.dialog_hints = default_dialog_hints();
    open_help(&mut app);
    let view = app.help.as_mut().expect("overlay open");
    view.state.open(&norte_help::TopicId::new("copying"));
    view.state.toggle_focus();
    // And it MOVES through the executable rows: since `Tab` respects where
    // the reader is (leaves the view still and brings the cursor to it),
    // reaching the command rows is what whoever wants to see them does — a
    // cursor movement, not a side effect of switching columns.
    view.state.up();
    refresh_help_en(&mut app, 100, 30);

    let mut terminal = Terminal::new(TestBackend::new(100, 30)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let text = terminal.backend().to_string();
    let buffer = terminal.backend().buffer().clone();

    let reason = norte_i18n::t_in(norte_i18n::Lang::Es, "reason-read-only");
    assert!(
        text.contains(&reason),
        "the vetoed row has to say WHY ({reason}):\n{text}"
    );

    // And the OTHER half of the function: the row is DIMMED. Saying the
    // reason on a row that still paints as clickable is half a feature, and
    // is the half the eye reads first — the text dump carries no styles, so
    // this is checked cell by cell.
    //
    // The TWO stretches `row_line` decides separately are checked: the
    // chord (`Mark` if clickable, `Info` if not — a dimmed row cannot dress
    // as a key) and the text. Only the FOREGROUND color: the background is
    // set by the overlay's block and says nothing about availability.
    let rows = row_texts(&buffer);
    let styles = all_row_styles(&buffer);
    let y = rows
        .iter()
        .position(|f| f.contains(&reason))
        .expect("the row with the reason falls inside the frame");
    let fg_de = |x: usize| {
        styles[y][x]
            .fg
            .expect("every painted cell has a foreground")
    };
    let en = |needle: &str| -> usize {
        let byte = rows[y].find(needle).expect("the chunk is in the row");
        rows[y][..byte].chars().count()
    };
    let dimmed = app.theme.role(norte_theme::Role::Info).fg;
    let normal = app.theme.role(norte_theme::Role::Regular).fg;
    let key = app.theme.role(norte_theme::Role::Mark).fg;
    assert!(
        dimmed != normal && dimmed != key,
        "the theme has to distinguish the three roles or this proves nothing"
    );

    let x_reason = en(&reason);
    for x in x_reason..x_reason + reason.chars().count() {
        assert_eq!(
            Some(fg_de(x)),
            dimmed,
            "the row says the reason but paints as if it were clickable: {:?}",
            rows[y]
        );
    }
    let x_chord = en("F5");
    assert_eq!(
        Some(fg_de(x_chord)),
        dimmed,
        "a vetoed row's chord cannot stay dressed as a key: {:?}",
        rows[y]
    );
    assert_ne!(Some(fg_de(x_chord)), key);
}

#[test]
fn snapshot_help_body_with_focus() {
    use norte_help::ChordResolver;

    let mut app = app_base();
    open_help(&mut app);
    let view = app.help.as_mut().expect("overlay open");
    view.state.open(&norte_help::TopicId::new("copying"));
    view.state.toggle_focus();
    assert_eq!(
        view.state.focus(),
        norte_frontend::help::Focus::Body,
        "the topic has executable rows, so focus DOES enter"
    );
    // One step: the SECOND row, so this cannot pass with a painter that
    // always highlights the first.
    view.state.down();
    let Some(norte_frontend::help::Action::Run(command)) = view.state.action().cloned() else {
        panic!("the focused row is an executable row");
    };
    assert_eq!(command, "pane.move", "the focused row is `pane.move`'s");
    refresh_help(&mut app);

    let scroll = app.help.as_ref().unwrap().state.body_scroll();
    assert!(
        scroll > 0,
        "the rows go after the prose: revealing them FORCES the body to \
         scroll (scroll={scroll})"
    );
    let with_focus = render_buffer(&app);
    let text = render(&app);

    // And the highlight belongs to FOCUS, not to the row: with focus
    // returned to the sidebar, the body's cursor still exists but is no
    // longer the one the arrows move, and no row stays marked. The same
    // gesture serves as the PATTERN for locating the highlighted row:
    // nothing else changes between the two frames (`reveal` no longer moves
    // the scroll, which this test just pinned).
    app.help.as_mut().unwrap().state.toggle_focus();
    refresh_help(&mut app);
    let unfocused = render_buffer(&app);

    let estilos_con = all_row_styles(&with_focus);
    let estilos_sin = all_row_styles(&unfocused);
    let different: Vec<usize> = (0..estilos_con.len())
        .filter(|&y| estilos_con[y] != estilos_sin[y])
        .collect();
    assert_eq!(
        different.len(),
        1,
        "exactly ONE row of the frame changes when focus leaves the body; \
         {different:?} changed"
    );

    // And that row is the one for the command the MODEL says has focus. The
    // chord and the label come from the resolver the painter uses, so this
    // cannot pass with a copy of the row format that has fallen behind.
    let resolver = std::sync::Arc::clone(&app.help_chords);
    let chord = resolver
        .chord(&command)
        .unwrap_or_else(|| panic!("{command} has a chord in the orthodox preset"));
    // The label may come out CLIPPED ("move the selection to the other …"):
    // the key column measures whatever the theme's widest one measures, and
    // since `alt+A` paints as `Alt+Shift+A` the label has fewer columns left
    // on an 80-wide terminal. What identifies the row is how it starts.
    let startup = |content: &str| content.chars().take(16).collect::<String>();
    let label = startup(&resolver.label(&command));
    let row = &row_texts(&with_focus)[different[0]];
    assert!(
        row.contains(&chord) && row.contains(&label),
        "the highlighted row has to be `{command}`'s ({chord} / \
         {label}…), not another one: {row:?}"
    );
    // …and NOT its neighbor's. A map shifted by one position would highlight
    // `pane.copy` while `Enter` dispatches `pane.move`.
    let neighbor = "pane.copy";
    let chord_neighbor = resolver
        .chord(neighbor)
        .unwrap_or_else(|| panic!("{neighbor} has a chord in the orthodox preset"));
    assert!(
        !row.contains(&startup(&resolver.label(neighbor))) && !row.contains(&chord_neighbor),
        "the highlighted row is the NEIGHBORING action's: the action→line \
         map is shifted: {row:?}"
    );

    insta::assert_snapshot!(text);
}

/// Command palette (`Ctrl+P`/vim `:`, H1 T4): filtered to "principio"
/// leaves TWO rows visible (`cursor.top`/`viewer.top` — both "go to the
/// start" in ES) with their real chord from the orthodox preset — the SAME
/// builder the binary uses (`norte_tui::palette::build_rows`), not a copy of
/// the format.
#[test]
fn snapshot_palette_open() {
    let mut app = app_base();
    let presets = norte_tui::keymap::presets();
    let (_, preset) = presets.iter().find(|(n, _)| *n == "orthodox").unwrap();
    let build = |screen| {
        norte_tui::keymap::Effective::build_for(preset, &[], norte_tui::keymap::COMMANDS, screen)
            .unwrap()
    };
    let rows = norte_tui::palette::build_rows(
        &build(norte_tui::keymap::Screen::Browse),
        &build(norte_tui::keymap::Screen::Viewer),
    );
    let mut palette = norte_tui::app::Palette::new(rows);
    for c in "principio".chars() {
        palette.push_char(c);
    }
    app.palette = Some(palette);
    insta::assert_snapshot!(render(&app));
}

/// P1: a plugin command row (`palette::plugin_rows`) filtered down to JUST
/// it — with a hostile title (RTL override, `rtl_override` corpus), to check
/// that neither the title nor the `[extension]` prefix paint raw, and that
/// the row stays distinguishable from a built-in.
#[test]
fn snapshot_palette_hostile_plugin_row() {
    let hostile = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("corpus fixture");
    let title = String::from_utf8_lossy(&hostile.bytes).into_owned();
    let mut app = app_base();
    let plugin = norte_proto::methods::PluginInfo {
        id: "org.evil.demo".into(),
        name: "Evil".into(),
        publisher: "evil".into(),
        version: "1.0.0".into(),
        category: "command".into(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: vec![norte_proto::methods::PluginCommandInfo {
            id: "run".into(),
            title,
            kind: norte_proto::methods::PluginCommandKind::Command,
        }],
        columns: Vec::new(),
        panels: Vec::new(),
        has_help: false,
        manifest_digest: None,
    };
    // No query: `plugin_rows` over ONE plugin with ONE command already
    // leaves a single row — "filtered down to just it" by construction, not
    // by typed text (the hostile title has no reason to contain anything
    // searchable).
    let rows = norte_tui::palette::plugin_rows(std::slice::from_ref(&plugin));
    let palette = norte_tui::app::Palette::new(rows);
    app.palette = Some(palette);
    let text = render(&app);
    // See the equivalent comment in
    // `snapshot_extensions_description_hostile_80x24`: the check is on the
    // INJECTED CHARACTER, not on "no control anywhere on the screen"
    // (`to_string()`'s line breaks are also controls, a false positive if
    // the whole buffer is scanned).
    assert!(
        !text.contains('\u{202E}'),
        "the title's RTL override painted raw: {text}"
    );
    assert!(
        text.contains('\u{FFFD}'),
        "the hostile title must be masked to U+FFFD: {text}"
    );
    insta::assert_snapshot!(text);
}

fn empty_cfg() -> norte_tui::config::LoadedConfig {
    norte_tui::config::load(&norte_config::Layers { dirs: vec![] }).expect("empty config loads")
}

/// Settings overlay (S3), opened over the EMPTY config (S2's default
/// values) — the SAME builder the binary uses
/// (`norte_tui::settings::build_rows`), not a hand-written copy. At 80×24:
/// the full catalogue (9 general rows + the Plugins informational one, plus
/// two section headers) does not fit in the 16 rows of the rest of the
/// file.
#[test]
fn snapshot_settings_open() {
    let mut app = app_base();
    let settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&empty_cfg(), &[]));
    app.settings = Some(settings);
    insta::assert_snapshot!(render_80x24(&app));
}

/// The settings list FOLLOWS the cursor when it does not fit.
///
/// It used to always paint from the top because "settings fits on one
/// screen" — that is what the shortcuts editor said, and it was true with
/// nine settings. With ~30 it stopped being true: scrolling past the edge
/// took the cursor out of the box and the list did not move. It was seen on
/// a real terminal, not here: the snapshot above is made with the cursor on
/// the first row, which is exactly where it does not fail.
///
/// It goes through `before_frame`, like the loop: that is what reconciles
/// the window, and a test that painted without it would be checking a
/// screen nobody sees.
#[test]
fn the_settings_list_follows_the_cursor() {
    let mut app = app_base();
    let mut settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&empty_cfg(), &[]));
    let ultima = settings.visible().len() - 1;
    settings.set_cursor(ultima);
    let entry_name = settings.rows()[settings.visible()[ultima]].name.clone();
    app.settings = Some(settings);
    ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, 80, 24));
    let screen = render_80x24(&app);
    assert!(
        screen
            .lines()
            .any(|l| l.contains(&format!("> {entry_name}"))),
        "the cursor's row (\"{entry_name}\") has to be visible, with its mark:\n{screen}"
    );
    // And the window has moved: the first row no longer fits.
    assert!(
        !screen.contains("Theme                        default")
            && !screen.contains("Tema                         default"),
        "with the cursor at the end, the first row scrolls off the top:\n{screen}"
    );
}

/// And the section header COMES BACK when scrolling up.
///
/// What Oscar saw: "if I scroll all the way down and back up, General does
/// not come back up." The window anchors to the cursor in LINES, and the
/// first row lives on line 1 because line 0 is the header: scrolling all
/// the way up leaves the offset at 1 and the header never reappears. The
/// rule that fixes it is more general than this one case — if the cursor is
/// on the FIRST row of its section, that section's header enters the window
/// with it — and this pins it by its symptom.
///
/// "General" no longer exists: the 33 entries were split into seven
/// sections. The anchor rule still holds, and is still the one that governs
/// headers that SCROLL — the one pinned at the top is a different piece.
#[test]
fn the_section_header_comes_back_on_scroll_up() {
    let mut app = app_base();
    let mut settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&empty_cfg(), &[]));
    let ultima = settings.visible().len() - 1;
    settings.set_cursor(ultima);
    app.settings = Some(settings);
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    // Scrolling all the way down moves the window...
    ui::before_frame(&mut app, area);
    // ...and going back up has to return it WHOLE, header included.
    app.settings.as_mut().expect("settings").set_cursor(0);
    ui::before_frame(&mut app, area);
    let screen = render_80x24(&app);
    let first = norte_i18n::t("settings-section-appearance");
    assert!(
        screen.contains(&first),
        "going back to the top has to show which section the row belongs to:\n{screen}"
    );
}

/// The cursor's section header stays PINNED at the top: it shows wherever
/// you are inside it, and changes when you cross into the next one.
///
/// It is the piece that makes the bug Oscar saw ("General does not come
/// back up") impossible: the label that says where you are stops depending
/// on scroll.
#[test]
fn the_pinned_header_follows_the_cursors_section() {
    let mut app = app_base();
    app.settings = Some(norte_tui::app::Settings::new(
        norte_tui::settings::build_rows(&empty_cfg(), &[]),
    ));
    let area = ratatui::layout::Rect::new(0, 0, 80, 24);
    ui::before_frame(&mut app, area);
    let up = render_80x24(&app);
    let first = norte_i18n::t("settings-section-appearance");
    assert!(
        up.contains(&first),
        "at the top the first section (\"{first}\") governs:\n{up}"
    );

    // All the way to the last row: the pinned one has to be a DIFFERENT one.
    let ultima = app.settings.as_ref().expect("settings").visible().len() - 1;
    app.settings.as_mut().expect("settings").set_cursor(ultima);
    ui::before_frame(&mut app, area);
    let down = render_80x24(&app);
    let ultima_section = norte_i18n::t("settings-section-plugins");
    assert!(
        down.contains(&ultima_section),
        "at the end its own section (\"{ultima_section}\") governs:\n{down}"
    );
}

/// With room, the index is there; without it, it goes away. The same
/// degradation a pane's columns do: a column that does not fit does not
/// shrink until illegible, it goes away.
#[test]
fn the_section_index_disappears_on_a_narrow_terminal() {
    let mut app = app_base();
    app.settings = Some(norte_tui::app::Settings::new(
        norte_tui::settings::build_rows(&empty_cfg(), &[]),
    ));
    let open_with = norte_i18n::t("settings-section-open-with");

    let wide = ratatui::layout::Rect::new(0, 0, 110, 24);
    ui::before_frame(&mut app, wide);
    let screen = render_at(&app, 110, 24);
    assert!(
        screen.contains(&open_with),
        "the index lists the sections the cursor has not visited:\n{screen}"
    );

    let is_narrow = ratatui::layout::Rect::new(0, 0, 50, 24);
    ui::before_frame(&mut app, is_narrow);
    let screen = render_at(&app, 50, 24);
    assert!(
        !screen.contains(&open_with),
        "at 50 columns the index does not fit and the list rules:\n{screen}"
    );
}

/// `tab` switches sides, and BOTH cursors are always visible: the one that
/// does not have the keyboard, dimmed. The same rule as help's two halves —
/// two live cursors, or none, is what makes you lose track of where you are.
#[test]
fn tab_switches_sides_and_the_arrows_walk_sections() {
    let mut app = app_base();
    app.settings = Some(norte_tui::app::Settings::new(
        norte_tui::settings::build_rows(&empty_cfg(), &[]),
    ));
    let s = app.settings.as_mut().expect("settings");
    assert_eq!(s.focus(), norte_frontend::settings::Focus::List);

    s.toggle_focus();
    assert_eq!(s.focus(), norte_frontend::settings::Focus::Index);
    // In the index, scrolling down changes SECTION and the list follows.
    s.down();
    let section = s.rows()[s.visible()[s.cursor()]].section;
    assert_eq!(section, norte_frontend::settings::Section::Panes);

    // And the screen shows it: the pinned header is the new section's.
    ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, 100, 24));
    let screen = render_at(&app, 100, 24);
    assert!(
        screen.contains(&norte_i18n::t("settings-section-panes")),
        "the screen follows the index:\n{screen}"
    );
}

/// The filter count. Without the second figure, "there is nothing" and "I
/// covered it with one letter" read the same.
#[test]
fn the_footer_says_how_many_settings_show_out_of_how_many() {
    let mut app = app_base();
    let settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&empty_cfg(), &[]));
    let total = settings.total();
    app.settings = Some(settings);
    ui::before_frame(&mut app, ratatui::layout::Rect::new(0, 0, 80, 24));
    let screen = render_80x24(&app);
    assert!(
        screen.contains(&total.to_string()),
        "the total ({total}) is always stated:\n{screen}"
    );
}

/// The 2026-09-18 capture: two ~50-column panes with Type, Size and Date
/// left 16 cells for the name and every screenshot came out as
/// `Ca….png`. The name now reads whole: the class yields, since the row
/// already states it, and the date switches to short.
#[test]
fn long_names_read_whole() {
    let dir = vp("file:///capturas");
    let entries = (0..5)
        .map(|i| {
            entry(
                &dir,
                format!("Captura de pantalla 202{i}.png").as_bytes(),
                EntryKind::File,
                Some(80_000),
            )
        })
        .collect();
    let mut app = App::new(Pane::new(dir.clone(), entries), Pane::new(dir, Vec::new()));
    app.dialog_hints = default_dialog_hints();
    app.columns = norte_frontend::columns::ColumnsSettings::resolve(&norte_config::ColumnsConfig {
        default_columns: Some(
            ["name", "size", "mtime", "kind"]
                .map(str::to_owned)
                .to_vec(),
        ),
        ..Default::default()
    });
    let mut terminal = Terminal::new(TestBackend::new(100, 16)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let screen = terminal.backend().to_string();
    // Only the LEFT pane: the right one is empty, has no names to read, and
    // so keeps all of its columns.
    let left: String = screen
        .lines()
        .map(|l| l.chars().take(51).collect::<String>() + "\n")
        .collect();
    assert!(
        left.contains("Captura de pantalla 2024.png"),
        "the whole name, no ellipsis:\n{screen}"
    );
    assert!(
        !left.contains("Tipo"),
        "the class is the first thing to yield:\n{screen}"
    );
    assert!(left.contains("Tamaño"), "the size stays:\n{screen}");
}

/// Menus go in sections (ADR 0125): Operate paints its labels, and the
/// "Delete" row — below two rules — runs Delete, not the command that would
/// fall on that row if the rules did not count.
#[test]
fn the_menu_paints_sections_and_the_click_follows_the_command() {
    let mut app = app_base();
    let operate = norte_frontend::menu::MENUS
        .iter()
        .position(|m| m.title == "menu-operate")
        .expect("Operar");
    let mut m = norte_frontend::menu::MenuState::new();
    m.open(operate);
    app.menu = Some(m);
    let area = ratatui::layout::Rect::new(0, 0, 80, 32);
    let mut terminal = Terminal::new(TestBackend::new(80, 32)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let screen = terminal.backend().to_string();
    assert!(
        screen.contains("├─ Archivos comprimidos"),
        "the section's label:\n{screen}"
    );
    let row = screen
        .lines()
        .position(|l| l.contains("Borrar ") && !l.contains("permanente"))
        .expect("the Delete row");
    let delete = norte_frontend::menu::MENUS[operate]
        .items()
        .position(|id| id == "pane.delete")
        .expect("Delete in Operate");
    let zone = ui::menu_zones(&app, area)
        .into_iter()
        .find(|z| usize::from(z.row) == row)
        .expect("the Delete row is clickable");
    assert_eq!(zone.hit, ui::MenuHit::Item(delete));
}

/// S review, M3: with the settings overlay AND a modal BOTH open (key
/// routing already treats the modal as AUTHORITATIVE in this case,
/// `modal_preempts_settings`), the modal must paint ON TOP — it used to
/// paint before `draw_settings` in `ui::draw`, so the overlay covered it
/// visually even though the keys still went to the modal. Pin: the modal's
/// title ("trash") is visible in the snapshot, not buried under the
/// settings list.
#[test]
fn snapshot_modal_paints_over_the_settings_overlay() {
    let mut app = app_base();
    let settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&empty_cfg(), &[]));
    app.settings = Some(settings);
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: false,
    });
    insta::assert_snapshot!(render_80x24(&app));
}

/// Same case as above, with the PALETTE instead of the settings overlay
/// (`modal_preempts_palette`) — the other half of H1 MINOR-4's class.
#[test]
fn snapshot_modal_paints_over_the_palette() {
    let mut app = app_base();
    let presets = norte_tui::keymap::presets();
    let (_, preset) = presets.iter().find(|(n, _)| *n == "orthodox").unwrap();
    let build = |screen| {
        norte_tui::keymap::Effective::build_for(preset, &[], norte_tui::keymap::COMMANDS, screen)
            .unwrap()
    };
    let rows = norte_tui::palette::build_rows(
        &build(norte_tui::keymap::Screen::Browse),
        &build(norte_tui::keymap::Screen::Viewer),
    );
    app.palette = Some(norte_tui::app::Palette::new(rows));
    app.modal = Some(Modal::ConfirmDelete {
        items: vec![vp("file:///casa/notas.txt")],
        permanent: false,
    });
    insta::assert_snapshot!(render(&app));
}

/// Filtered + EDITING (S3): filters down to the `Text` `ui.font` row (the
/// trailing space isolates it from `ui.font-size`, see
/// `app::settings_tests`), `activate` opens the edit buffer and a value is
/// typed — checks that the filtered list, the description and the edit
/// footer (`settings-edit-hint`) paint together without stepping on each
/// other.
#[test]
fn snapshot_settings_filtered_and_editing_text() {
    let mut app = app_base();
    let mut settings =
        norte_tui::app::Settings::new(norte_tui::settings::build_rows(&empty_cfg(), &[]));
    for c in "ui.font ".chars() {
        settings.push_char(c);
    }
    settings.activate(&[], &[]);
    for c in "Fira Code".chars() {
        settings.edit_push_char(c);
    }
    app.settings = Some(settings);
    insta::assert_snapshot!(render_80x24(&app));
}

/// The differences pane (`Shift+F2`, 2026-08-11-directory-comparison.md).
///
/// The model's suite lives in `norte-frontend` and paints nothing; what this
/// snapshot freezes is the COMPOSITION, which is the only thing that breaks
/// silently: that both sides fit, that both marks stay between them, that a
/// non-UTF8 name comes out masked and BADGED on both, and that the footer
/// states the status, the active side and the five counts.
///
/// There is a row for every category on purpose, including the pair that
/// differs only in CONFIDENCE (`= !` vs `= ~`): that distinction is the
/// whole item's reason to exist, and a render that lost it would still be
/// green on every other assertion.
/// A test comparison row: both sides come from the snapshot's two roots, and
/// `reason` is filled in only where the wire requires it
/// (`CompareRow::reason_is_consistent`).
fn row_compare(
    id: u64,
    entry_name: &[u8],
    verdict: norte_proto::methods::CompareVerdict,
    criterion: norte_proto::methods::CompareCriterion,
    confidence: norte_proto::methods::CompareConfidence,
    left_side: bool,
    right_side: bool,
) -> norte_proto::methods::CompareRow {
    use norte_proto::methods::{CompareReason, CompareRow, CompareVerdict};
    let left = vp("file:///casa");
    let right = vp("file:///otro");
    CompareRow {
        id,
        left: left_side.then(|| entry(&left, entry_name, EntryKind::File, Some(1024))),
        right: right_side.then(|| entry(&right, entry_name, EntryKind::File, Some(2048))),
        verdict,
        criterion,
        confidence,
        newer: None,
        reason: matches!(verdict, CompareVerdict::Ambiguous | CompareVerdict::Error)
            .then_some(CompareReason::Unreadable),
        side: None,
        paired_under: None,
    }
}

#[test]
fn snapshot_compare_pane() {
    use norte_proto::methods::{CompareConfidence, CompareCriterion, CompareVerdict};

    let left = vp("file:///casa");
    let right = vp("file:///otro");
    let row = row_compare;

    let mut view = norte_tui::app::CompareView::new(left, right, 0, None, None);
    view.pane.extend(vec![
        // Proven by the hash, and only suggested by the date: TWO answers.
        row(
            1,
            b"probado.bin",
            CompareVerdict::Same,
            CompareCriterion::Hash,
            CompareConfidence::Certain,
            true,
            true,
        ),
        row(
            2,
            b"supuesto.bin",
            CompareVerdict::Same,
            CompareCriterion::Mtime,
            CompareConfidence::Probable,
            true,
            true,
        ),
        // A provider that cannot tell (a .zip): an answer, not a failure.
        row(
            3,
            b"en-archivo.txt",
            CompareVerdict::Same,
            CompareCriterion::Mtime,
            CompareConfidence::Unknown,
            true,
            true,
        ),
        row(
            4,
            b"distinto.txt",
            CompareVerdict::Different,
            CompareCriterion::Size,
            CompareConfidence::Certain,
            true,
            true,
        ),
        row(
            5,
            &[0xE9, b'.', b'd', b'a', b't'],
            CompareVerdict::OnlyLeft,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
            true,
            false,
        ),
        row(
            6,
            b"solo-derecha",
            CompareVerdict::OnlyRight,
            CompareCriterion::Presence,
            CompareConfidence::Certain,
            false,
            true,
        ),
        row(
            7,
            b"clase-distinta",
            CompareVerdict::TypeMismatch,
            CompareCriterion::Kind,
            CompareConfidence::Certain,
            true,
            true,
        ),
        // C6, finding 7: an `Error` may carry NEITHER side.
        row(
            8,
            b"ilegible",
            CompareVerdict::Error,
            CompareCriterion::Presence,
            CompareConfidence::Unknown,
            false,
            false,
        ),
    ]);
    view.state = norte_tui::app::CompareState::Done;
    view.pane.select(4);

    let mut app = app_base();
    app.compare = Some(view);
    insta::assert_snapshot!(render_80x24(&app));
}

/// A test sync-plan step.
fn paso_sync(
    id: u64,
    kind: norte_proto::methods::SyncStepKind,
    rel: &str,
    size: Option<u64>,
    reversal: norte_proto::methods::StepReversal,
) -> norte_proto::methods::SyncStep {
    use norte_proto::methods::{
        CompareConfidence, CompareCriterion, RelPath, StepReversal, SyncReason, SyncStep,
    };
    let step = SyncStep {
        id,
        kind,
        rel: RelPath::parse_wire(rel).expect("rel"),
        dest_rel: None,
        size,
        criterion: CompareCriterion::Size,
        confidence: CompareConfidence::Certain,
        reversal: Some(reversal),
        // `shape_is_consistent`: an IRREVERSIBLE step must say why, and here
        // the why is always the same — the destination cannot give it back.
        reason: (reversal == StepReversal::Irreversible).then_some(SyncReason::NoTrashOnTarget),
    };
    assert!(step.shape_is_consistent(), "malformed test step: {step:?}");
    step
}

fn close_sync(
    counts: norte_proto::methods::SyncCounts,
    dest_trash: norte_proto::methods::DestTrash,
) -> norte_proto::methods::SyncPlanDone {
    norte_proto::methods::SyncPlanDone {
        task_id: norte_proto::TaskId::new(1),
        plan_hash: norte_proto::methods::PlanHash::from_digest(&[7u8; 32]),
        counts,
        blockers: Vec::new(),
        blockers_total: 0,
        executable: true,
        dest_trash,
    }
}

/// The sync pane of an `Update` against a destination whose trash DOES say
/// where it buries things: the normal case on Linux, and the only one a
/// development machine can really produce (`os_root` always declares
/// `trash_restorable`).
///
/// What this snapshot freezes is the COMPOSITION at 80 columns: that the
/// title fits with both roots and the DIRECTION arrow, that the summary does
/// not eat into the list, that each step's three marks stay separated, and
/// that the key line does not cut off mid-word — which is exactly the
/// failure the differences pane's line had when these keys were added to
/// it.
#[test]
fn snapshot_sync_pane_update_with_trash() {
    use norte_proto::methods::{DestTrash, StepReversal, SyncCounts, SyncMode, SyncStepKind};

    let mut view = norte_tui::app::SyncView::new(
        norte_proto::TaskId::new(1),
        SyncMode::Update,
        vp("file:///casa"),
        vp("file:///otro"),
        None,
        None,
    );
    let steps = vec![
        paso_sync(
            1,
            SyncStepKind::CreateDir,
            "sub",
            None,
            StepReversal::Delete,
        ),
        paso_sync(
            2,
            SyncStepKind::Copy,
            "sub/c.txt",
            Some(2048),
            StepReversal::Delete,
        ),
        paso_sync(
            3,
            SyncStepKind::Overwrite,
            "a.txt",
            Some(4096),
            StepReversal::RestoreTrash,
        ),
        paso_sync(
            4,
            SyncStepKind::Copy,
            "nuevo.txt",
            None,
            StepReversal::Delete,
        ),
    ];
    let counts = SyncCounts {
        create_dir: 1,
        copy: 2,
        overwrite: 1,
        delete_tree: 0,
        skip: 0,
        irreversible: 0,
        bytes: 6144,
        unmeasured_steps: 1,
        unknown_kind: 0,
    };
    view.state =
        norte_frontend::sync::SyncState::ready(steps, close_sync(counts, DestTrash::Restorable));
    view.run = norte_tui::app::SyncRunState::Done;

    let mut app = app_base();
    app.sync = Some(view);
    insta::assert_snapshot!(render_80x24(&app));
}

/// And the case this machine CANNOT produce: a `Mirror` that deletes a tree
/// against a destination WITHOUT trash, with the second question open.
///
/// `norte-vfs-local` always declares `TRASH` and, with the root at `/`,
/// always answers `trash_restorable` — so `Opaque` and `Absent` are only
/// reachable on macOS and on Windows, and here only by construction. This
/// snapshot is what makes the sentence get read, and what stops a change to
/// the summary from leaving "nothing can be undone" off screen in the one
/// case where not reading it costs data.
#[test]
fn snapshot_sync_pane_mirror_without_trash() {
    use norte_proto::methods::{DestTrash, StepReversal, SyncCounts, SyncMode, SyncStepKind};

    let mut view = norte_tui::app::SyncView::new(
        norte_proto::TaskId::new(1),
        SyncMode::Mirror,
        vp("file:///casa"),
        vp("file:///otro"),
        None,
        None,
    );
    let steps = vec![
        paso_sync(
            1,
            SyncStepKind::Copy,
            "nuevo.txt",
            Some(64),
            StepReversal::Delete,
        ),
        paso_sync(
            2,
            SyncStepKind::Overwrite,
            "a.txt",
            Some(4096),
            StepReversal::Irreversible,
        ),
        paso_sync(
            3,
            SyncStepKind::DeleteTree,
            "arbol-sobrante",
            None,
            StepReversal::Irreversible,
        ),
        paso_sync(
            4,
            SyncStepKind::DeleteTree,
            "sobra.txt",
            None,
            StepReversal::Irreversible,
        ),
    ];
    let counts = SyncCounts {
        create_dir: 0,
        copy: 1,
        overwrite: 1,
        delete_tree: 2,
        skip: 0,
        irreversible: 3,
        bytes: 4160,
        unmeasured_steps: 0,
        unknown_kind: 0,
    };
    view.state =
        norte_frontend::sync::SyncState::ready(steps, close_sync(counts, DestTrash::Absent));
    view.run = norte_tui::app::SyncRunState::Done;
    view.confirming = view
        .state
        .plan()
        .expect("plan")
        .confirmation(norte_i18n::active());
    assert!(
        view.confirming.is_some(),
        "a mirror that deletes two trees with no trash HAS to ask twice"
    );

    let mut app = app_base();
    app.sync = Some(view);
    insta::assert_snapshot!(render_80x24(&app));
}
