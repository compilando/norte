//! Snapshots del render (fase 10, insta): la forma EXACTA de cada pantalla
//! queda congelada — cualquier cambio visual es un diff consciente
//! (`cargo insta review`). Determinista: idioma fijo, datos fijos.

use norte_core::TransferOptions;
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{App, Modal, Pane, TransferKind, sort_entries};
use norte_tui::tasks::RetrySpec;
use norte_tui::ui;
use norte_tui::viewer::Viewer;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entry(dir: &VPath, name: &[u8], kind: EntryKind, size: Option<u64>) -> Entry {
    Entry {
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

fn app_base() -> App {
    let izq = vp("file:///casa");
    let der = vp("file:///otro");
    let mut entries = vec![
        entry(&izq, b"docs", EntryKind::Dir, None),
        entry(&izq, b"src", EntryKind::Dir, None),
        entry(&izq, b"notas.txt", EntryKind::File, Some(420)),
        entry(
            &izq,
            &[0xE9, b'.', b'd', b'a', b't'],
            EntryKind::File,
            Some(7),
        ),
        entry(&izq, b"enlace", EntryKind::Symlink, None),
    ];
    sort_entries(&mut entries);
    let mut app = App::new(
        Pane::new(izq, entries),
        Pane::new(
            der.clone(),
            vec![entry(&der, b"cosa", EntryKind::File, Some(1))],
        ),
    );
    app.focused_mut().move_down(1);
    app
}

#[test]
fn snapshot_navegacion() {
    insta::assert_snapshot!(render(&app_base()));
}

#[test]
fn snapshot_modal_colision() {
    let mut app = app_base();
    app.modal = Some(Modal::Collision {
        retry: RetrySpec {
            kind: TransferKind::Copy,
            from: vp("file:///casa/notas.txt"),
            to: vp("file:///otro/notas.txt"),
            opts: TransferOptions::default(),
        },
    });
    insta::assert_snapshot!(render(&app));
}

#[test]
fn snapshot_modal_papelera_y_permanente() {
    let mut app = app_base();
    app.modal = Some(Modal::ConfirmDelete {
        target: vp("file:///casa/notas.txt"),
        permanent: false,
    });
    let papelera = render(&app);
    app.modal = Some(Modal::ConfirmDelete {
        target: vp("file:///casa/notas.txt"),
        permanent: true,
    });
    let permanente = render(&app);
    insta::assert_snapshot!(format!("{papelera}\n===\n{permanente}"));
}

#[test]
fn snapshot_viewer_texto_y_hex() {
    let mut app = app_base();
    app.viewer = Some(Viewer::new(
        vp("file:///casa/notas.txt"),
        b"a\xF1o 2026\nsegunda l\xEDnea\n".to_vec(),
        false,
    ));
    let texto = render(&app);
    let mut v = Viewer::new(
        vp("file:///casa/logo.png"),
        b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec(),
        false,
    );
    v.scroll_down(0);
    app.viewer = Some(v);
    let hex = render(&app);
    insta::assert_snapshot!(format!("{texto}\n===\n{hex}"));
}
