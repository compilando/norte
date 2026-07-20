//! Tests del estado puro del TUI (fase 3 M1): navegación, cd, sort y
//! display de nombres hostiles. Cero terminal: el estado es una máquina
//! pura sobre `Entry`s.

use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{Pane, display_name, sort_entries};

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido de test")
}

fn entry(dir: &VPath, name: &[u8], kind: EntryKind) -> Entry {
    Entry {
        path: dir.join(Segment::new(name.to_vec()).expect("segmento válido")),
        kind,
        size: (kind == EntryKind::File).then_some(42),
        mtime_ms: None,
    }
}

fn pane_with(names: &[(&[u8], EntryKind)]) -> Pane {
    let dir = vp("file:///base");
    let mut entries: Vec<Entry> = names.iter().map(|(n, k)| entry(&dir, n, *k)).collect();
    sort_entries(&mut entries);
    Pane::new(dir, entries)
}

#[test]
fn sort_pone_dirs_primero_y_por_bytes() {
    let dir = vp("file:///base");
    let mut entries = vec![
        entry(&dir, b"zeta.txt", EntryKind::File),
        entry(&dir, b"alfa.txt", EntryKind::File),
        entry(&dir, b"carpeta", EntryKind::Dir),
        entry(&dir, b"Alfa", EntryKind::Dir),
        entry(&dir, b"enlace", EntryKind::Symlink),
    ];
    sort_entries(&mut entries);
    let names: Vec<&[u8]> = entries
        .iter()
        .map(|e| e.path.file_name().unwrap().as_bytes())
        .collect();
    // Dirs primero (orden de bytes: mayúsculas antes), luego el resto.
    assert_eq!(
        names,
        vec![
            b"Alfa".as_slice(),
            b"carpeta",
            b"alfa.txt",
            b"enlace",
            b"zeta.txt"
        ]
    );
}

#[test]
fn cursor_navega_con_topes() {
    let mut p = pane_with(&[
        (b"a", EntryKind::File),
        (b"b", EntryKind::File),
        (b"c", EntryKind::File),
    ]);
    assert_eq!(p.cursor(), 0);
    p.move_up(1);
    assert_eq!(p.cursor(), 0, "tope superior");
    p.move_down(1);
    assert_eq!(p.cursor(), 1);
    p.move_down(100);
    assert_eq!(p.cursor(), 2, "tope inferior");
    p.move_to_end();
    assert_eq!(p.cursor(), 2);
    p.move_to_start();
    assert_eq!(p.cursor(), 0);
}

#[test]
fn cursor_en_pane_vacio_no_revienta() {
    let mut p = pane_with(&[]);
    p.move_down(1);
    p.move_up(1);
    p.move_to_end();
    assert_eq!(p.cursor(), 0);
    assert!(p.selected().is_none());
}

#[test]
fn selected_devuelve_la_entrada_bajo_el_cursor() {
    let mut p = pane_with(&[(b"a", EntryKind::File), (b"dir", EntryKind::Dir)]);
    // Tras el sort: [dir, a].
    assert_eq!(
        p.selected().unwrap().path.file_name().unwrap().as_bytes(),
        b"dir"
    );
    p.move_down(1);
    assert_eq!(
        p.selected().unwrap().path.file_name().unwrap().as_bytes(),
        b"a"
    );
}

#[test]
fn display_marca_toda_perdida_y_neutraliza_controles() {
    // Nombre UTF-8 limpio: idéntico y sin badge.
    let (texto, hostil) = display_name(b"normal.txt");
    assert_eq!(texto, "normal.txt");
    assert!(!hostil);

    // Propiedad de la spec §6: badge EXACTAMENTE cuando el texto pintado
    // difiere del nombre real (lossy, controles enmascarados o bidi).
    for n in norte_testkit::corpus::hostile_names() {
        let (texto, hostil) = display_name(&n.bytes);
        assert!(!texto.is_empty(), "{}: display jamás vacío", n.id);
        let identico = texto.as_bytes() == n.bytes.as_slice();
        assert_eq!(
            hostil, !identico,
            "{}: badge exactamente cuando el display difiere del real",
            n.id
        );
        // Jamás controles crudos ni bidi override hacia el terminal:
        // ratatui los BORRARÍA en silencio (nombre visible ≠ real) y un
        // frontend directo ejecutaría ANSI / reordenaría RTL.
        assert!(
            !texto.chars().any(|c| c.is_control()
                || matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')),
            "{}: sin Cc ni Cf-bidi en el display",
            n.id
        );
        if !identico {
            assert!(
                texto.contains('\u{FFFD}'),
                "{}: la pérdida se ve (spec §6: lossy marcado)",
                n.id
            );
        }
    }

    // Un archivo REALMENTE llamado � (UTF-8 válido) no lleva badge: la
    // distinción con un lossy depende del badge, no del glifo.
    let (texto, hostil) = display_name("\u{FFFD}".as_bytes());
    assert_eq!(texto, "\u{FFFD}");
    assert!(!hostil);
}

#[test]
fn sort_junta_las_variantes_de_normalizacion() {
    // spec §6.1: unicode_compare = nfc por defecto PARA ORDENAR (los bytes
    // jamás se mutan). NFC y NFD del mismo nombre quedan adyacentes.
    let dir = vp("file:///base");
    let mut entries = vec![
        entry(&dir, &[0xC3, 0xA9], EntryKind::File), // é NFC
        entry(&dir, b"zzz", EntryKind::File),
        entry(&dir, &[0x65, 0xCC, 0x81], EntryKind::File), // é NFD
        entry(&dir, b"aaa", EntryKind::File),
    ];
    sort_entries(&mut entries);
    let names: Vec<&[u8]> = entries
        .iter()
        .map(|e| e.path.file_name().unwrap().as_bytes())
        .collect();
    // é (U+00E9) ordena tras 'z' por su clave NFC; lo que importa: las DOS
    // variantes quedan ADYACENTES (misma clave, desempate por bytes crudos:
    // NFD 0x65… < NFC 0xC3…). Sin clave NFC, "zzz" partiría el par.
    assert_eq!(names[0], b"aaa");
    assert_eq!(names[1], b"zzz");
    assert_eq!(names[2], &[0x65, 0xCC, 0x81][..]);
    assert_eq!(names[3], &[0xC3, 0xA9][..]);
}

#[test]
fn path_display_marca_paths_con_segmentos_hostiles() {
    use norte_tui::app::path_display;
    let limpio = vp("file:///casa/docs");
    let (texto, hostil) = path_display(&limpio);
    assert!(texto.contains("docs"));
    assert!(!hostil);

    let feo = limpio.join(Segment::new(vec![0xE9]).unwrap());
    let (_, hostil) = path_display(&feo);
    assert!(hostil, "un segmento no-UTF8 marca el path entero");
}

#[test]
fn tab_alterna_el_foco_entre_los_dos_panes() {
    use norte_tui::app::App;
    let mut app = App::new(pane_with(&[]), pane_with(&[]));
    assert_eq!(app.focus(), 0);
    app.switch_focus();
    assert_eq!(app.focus(), 1);
    app.switch_focus();
    assert_eq!(app.focus(), 0);
    app.focused_mut().move_down(1);
    assert!(!app.quit);
}
