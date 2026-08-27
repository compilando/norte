//! El panel de procesos: que las teclas LLEGUEN.
//!
//! Los tests que había afirmaban `key_owner()`, y eso es justamente lo que
//! dejó invisible el agujero de #243: el `KeyOwner` se ponía, el borde de foco
//! se pintaba, y ninguna tecla llegaba al panel — las flechas movían el
//! listado de detrás y F8 abría el diálogo de borrar sobre su selección.
//!
//! Aquí la tecla entra por donde entra de verdad: preset → `Effective` de la
//! pantalla `dialog` → `Resolver` → comando → despacho del panel.

use norte_frontend::keymap::{CATALOGUE, Chord, Effective, Resolution, Resolver, Screen, presets};
use norte_proto::{Entry, EntryKind, Segment, VPath};
use norte_tui::app::{ALLOW_PROCESSES, App, KeyOwner, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("wire válido")
}

fn entradas(dir: &VPath) -> Vec<Entry> {
    (0..3)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir
                .join(Segment::new(format!("f{i:02}").into_bytes()).expect("segmento"))
                .clone(),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        })
        .collect()
}

fn app_de_prueba() -> App {
    let dir = vp("file:///casa");
    App::new(
        Pane::new(dir.clone(), entradas(&dir)),
        Pane::new(dir.clone(), entradas(&dir)),
    )
}

/// El keymap efectivo de la pantalla `dialog` de un preset, que es el que
/// resuelve mientras el teclado está dentro de un panel.
fn dialogo(preset: &str) -> Effective {
    let conocidos: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    let src = presets::source(preset).expect("el preset existe");
    let kf = norte_frontend::keymap::parse_keymap(src).expect("el preset parsea");
    Effective::build_for(&kf, &[], &conocidos, Screen::Dialog).expect("el preset fusiona")
}

/// La SECUENCIA de teclas que un preset ata a `cmd` en la pantalla `dialog`,
/// disponible ahí.
fn tecla_de(eff: &Effective, cmd: &str) -> Vec<Chord> {
    eff.bindings_all_seq()
        .into_iter()
        .find(|(_, c, avail)| {
            *c == cmd && matches!(avail, norte_frontend::keymap::Availability::Here)
        })
        .map_or_else(
            || panic!("ningún preset ata {cmd} en dialog"),
            |(seq, _, _)| seq.to_vec(),
        )
}

/// Mete una tecla por el camino de verdad y devuelve lo que el panel diga.
fn pulsar(app: &mut App, resolver: &mut Resolver, seq: Vec<Chord>) -> Option<String> {
    let mut last = None;
    for chord in seq {
        match resolver.push(chord) {
            Resolution::Run { command, .. } => last = app.processes_command(&command),
            Resolution::Pending(_) | Resolution::Counting(_) => {}
            otro => panic!("la tecla no resuelve a un comando: {otro:?}"),
        }
    }
    last
}

/// Enter sobre el panel CANCELA, que es lo que el CHANGELOG y los dos temas
/// de ayuda llevaban prometiendo sin implementación detrás. Sin tareas dice
/// que no hay ninguna — lo que no puede hacer es caer al listado de detrás.
#[test]
fn confirmar_actua_sobre_el_panel_y_no_sobre_el_listado() {
    let eff = dialogo("orthodox");
    let mut resolver = Resolver::new(eff.clone());
    let mut app = app_de_prueba();
    app.toggle_processes();
    assert_eq!(
        app.key_owner(),
        KeyOwner::Processes,
        "el panel tiene teclas"
    );

    let msg = pulsar(&mut app, &mut resolver, tecla_de(&eff, "dialog.confirm"));
    assert_eq!(
        msg,
        Some(norte_i18n::t("msg-no-tasks")),
        "contesta el PANEL: sin tareas, no hay nada que cancelar"
    );
    assert!(app.modal.is_none(), "y no abre nada del listado de detrás");
}

/// Escape suelta el teclado sin cerrar el panel, y su propia tecla lo cierra
/// desde dentro: la tercera pulsación de abrir → enfocar → cerrar.
#[test]
fn cancelar_suelta_el_teclado_y_su_tecla_cierra_desde_dentro() {
    let eff = dialogo("orthodox");
    let mut resolver = Resolver::new(eff.clone());
    let mut app = app_de_prueba();

    app.toggle_processes();
    pulsar(&mut app, &mut resolver, tecla_de(&eff, "dialog.cancel"));
    assert_eq!(app.key_owner(), KeyOwner::Panes, "el teclado vuelve");
    assert!(
        app.processes_slot().is_some(),
        "pero el panel sigue abierto"
    );

    app.toggle_processes();
    assert_eq!(app.key_owner(), KeyOwner::Processes);
    // Por comando y no por tecla: NINGÚN preset ata hoy `layout.processes`
    // —el panel se abre por menú o paleta (#228)—, y lo que este test fija es
    // que el panel DESPACHA su propia tecla si alguien la ata. Sin eso, atarla
    // daría un panel que se abre y no se cierra, que es el agujero que el
    // sidebar de sitios ya tuvo.
    assert!(app.processes_command("layout.processes").is_none());
    assert!(app.processes_slot().is_none(), "cerrado desde dentro");
    assert_eq!(app.key_owner(), KeyOwner::Panes);
}

/// `Tab` devuelve el teclado a los listados sin cerrar el panel.
///
/// Abrir un panel con teclado no puede costarte la tecla con la que se cambia
/// de panel toda la vida. El panel se come lo que no esté en su allowlist, así
/// que `Tab` quedaba muerto mientras estuviera abierto.
///
/// Se prueban los DOS verbos porque esa tecla tiene dos nombres según la
/// pantalla: en la de diálogo los presets atan `tab` a `dialog.pane`, y en la
/// de navegar es `pane.switch`. La primera versión de este arreglo solo aceptó
/// el segundo, y por eso no hizo NADA con los presets tal y como se envían —
/// la suite pasaba y la tecla seguía muerta. Lo destapó pilotarlo en tmux.
#[test]
fn tab_devuelve_el_teclado_sin_cerrar_el_panel() {
    for verbo in ["dialog.pane", "pane.switch"] {
        let mut app = app_de_prueba();
        app.toggle_processes();
        assert_eq!(app.key_owner(), KeyOwner::Processes);

        assert!(app.processes_command(verbo).is_none());
        assert_eq!(
            app.key_owner(),
            KeyOwner::Panes,
            "«{verbo}» devuelve el teclado"
        );
        assert!(
            app.processes_slot().is_some(),
            "y el panel sigue abierto: salir no es cerrar"
        );
    }
}

/// El movimiento del cursor está en el allowlist Y despachado: sin las dos
/// cosas, el `▶` se queda en la fila 0 para siempre mientras las flechas
/// mueven otra lista.
#[test]
fn el_panel_despacha_su_vocabulario_entero() {
    for cmd in [
        "dialog.up",
        "dialog.down",
        "dialog.confirm",
        "dialog.cancel",
        "layout.processes",
    ] {
        assert!(ALLOW_PROCESSES.contains(&cmd), "{cmd} fuera del allowlist");
    }
    // Y lo que NO es suyo sigue siendo inerte aquí dentro: el panel no borra
    // ficheros.
    let mut app = app_de_prueba();
    app.toggle_processes();
    assert!(app.processes_command("pane.delete").is_none());
    assert!(app.modal.is_none(), "F8 no abre nada desde este panel");
}

/// El preset que ate `layout.processes` tiene que atarlo en las DOS
/// pantallas: con la tecla solo en `browse`, abrir el panel la haría
/// desaparecer —el teclado pasa a resolver por `dialog`— y el panel quedaría
/// abierto sin tecla que lo cierre. Es la lección que dejó el sidebar de
/// sitios, escrita antes de que ningún preset lo ate (hoy no lo ata ninguno:
/// se abre por menú o paleta, #228).
#[test]
fn el_preset_que_ate_layout_processes_lo_ata_en_las_dos_pantallas() {
    let conocidos: Vec<&str> = CATALOGUE.iter().map(|d| d.name).collect();
    for nombre in presets::NAMES {
        let src = presets::source(nombre).expect("el preset existe");
        let kf = norte_frontend::keymap::parse_keymap(src).expect("el preset parsea");
        let ata = |pantalla| {
            Effective::build_for(&kf, &[], &conocidos, pantalla)
                .expect("el preset fusiona")
                .bindings()
                .iter()
                .any(|(_, cmd)| *cmd == "layout.processes")
        };
        assert_eq!(
            ata(Screen::Browse),
            ata(Screen::Dialog),
            "{nombre} ata layout.processes en una pantalla y no en la otra"
        );
    }
}
