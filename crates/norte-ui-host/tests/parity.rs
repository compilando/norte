//! Paridad: el TERMINAL y la VENTANA hacen lo mismo, y las dos hacen lo que
//! dicen las primitivas compartidas.
//!
//! Ningún frontend debe reimplementar una regla de presentación. Eso es fácil
//! de decir en un comentario y difícil de mantener: basta un `sort` propio,
//! un clamp de cursor a mano o un «esto es más cómodo así» para que dos
//! superficies empiecen a leerse distinto sin que nada se ponga rojo.
//!
//! Cada escenario se ejecuta TRES veces —contra `norte_frontend::PaneState` +
//! `nav::History` a pelo, contra el host por sus acciones y sus fotos, y
//! contra `norte-tui` por sus propias funciones de decisión— y se compara el
//! estado SEMÁNTICO paso a paso: dónde está el cursor, qué hay marcado, qué
//! directorio se ve y con qué nombres. Nunca píxeles.
//!
//! **La tercera pata es la que guarda algo, y faltaba** (ADR 0097, D1). Las
//! dos primeras miden el host contra un arnés escrito con las reglas DEL
//! HOST: su paso `Entrar` era `selected().filter(|e| e.kind == Dir)`, que es
//! lo que hace la ventana y no lo que hace el terminal, así que la
//! comparación no podía fallar. La auditoría de paridad del 2026-09-05
//! encontró diecisiete decisiones ya divergidas por debajo de este fichero.
//!
//! El árbol de prueba ya trae un `.zip` y un symlink, que era la divergencia
//! número uno del inventario: `Enter` sobre uno de los dos navegaba en el
//! terminal y llamaba a `xdg-open` en la ventana. Las tres patas preguntan
//! ahora por `norte_frontend::nav::enter_target`, así que la pregunta «¿esto
//! se entra?» tiene UNA respuesta y los escenarios pueden ejercitarla.
//!
//! Lo que todavía NO alcanza: un arnés de paridad caza DIVERGENCIA, no error
//! compartido. Si las dos superficies se equivocan igual —porque las dos leen
//! la misma primitiva— aquí sale verde. Al añadir un escenario, sabotea una
//! sola de las patas y compruébalo rojo; estrechar la primitiva estrecha las
//! tres y no demuestra nada.

use std::sync::Arc;

use norte_frontend::PaneState;
use norte_frontend::nav::{History, Trail};
use norte_proto::{Entry, VPath};
use norte_ui_host::action::UiAction;
use norte_ui_host::dto::SlotView;
use norte_ui_host::{UiHost, UiHostOptions, UiSubscription, Update, ViewSnapshot, dto::UiUpdate};

mod backend_falso;
use backend_falso::{Falso, arbol_de_prueba};

/// Un paso del escenario, en vocabulario SEMÁNTICO: ni teclas ni acciones del
/// bridge, para que la comparación no dependa de por dónde entra cada
/// superficie.
#[derive(Debug, Clone, Copy)]
enum Paso {
    /// Mueve el cursor tantas filas.
    Cursor(i64),
    /// Pone el cursor en la fila que se llama así, bajando desde arriba.
    ///
    /// Por NOMBRE y no por índice porque `compara` corre cada escenario con
    /// la fila `..` puesta y quitada, y el índice de la misma entrada no es
    /// el mismo en las dos. Cada superficie lo hace con sus propias teclas de
    /// cursor: lo que se compara sigue siendo dónde acaba.
    CursorA(&'static str),
    /// Marca o desmarca la fila del cursor.
    Marcar,
    /// Entra en el directorio bajo el cursor.
    Entrar,
    /// Sube al padre.
    Subir,
    /// Atrás en el rastro.
    Atras,
    /// Adelante en el rastro.
    Adelante,
}

/// Lo que se compara: el estado que un usuario podría describir en voz alta.
#[derive(Debug, PartialEq, Eq)]
struct Semantico {
    dir: String,
    cursor: usize,
    marcas: usize,
    nombres: Vec<String>,
}

/// El escenario corrido contra las primitivas compartidas, a pelo.
fn via_primitivas(pasos: &[Paso], fila_de_subir: bool) -> Vec<Semantico> {
    let arbol = arbol_de_prueba();
    let inicio = VPath::parse("mem:///casa").expect("vpath");
    let mut pane = PaneState::new(inicio.clone(), entradas(&arbol, &inicio));
    pane.set_parent_row(fila_de_subir);
    let mut historial = History::default();
    let mut salida = vec![foto_primitivas(&pane)];

    for paso in pasos {
        match paso {
            Paso::Cursor(delta) => {
                for _ in 0..delta.unsigned_abs() {
                    if *delta < 0 {
                        pane.cursor_up();
                    } else {
                        pane.cursor_down();
                    }
                }
            }
            Paso::CursorA(nombre) => {
                for _ in 0..pane.entries().len() {
                    pane.cursor_up();
                }
                for _ in 0..pane.entries().len() {
                    if foto_primitivas(&pane).nombres.get(pane.cursor())
                        == Some(&(*nombre).to_owned())
                    {
                        break;
                    }
                    pane.cursor_down();
                }
            }
            Paso::Marcar => pane.toggle_mark(),
            Paso::Entrar => {
                // Sobre `..`, Enter SUBE: es lo único que esa fila sabe
                // hacer, y lo hacen las dos superficies —el TUI en
                // `trail::nav_enter_target` y el host en `UiAction::Activate`,
                // que ve la `Entry` sintética y navega a su ruta—. Modelarlo
                // como «no hay operando, no pasa nada» mediría el arnés y no
                // el producto.
                //
                // Y qué es «entrable» lo contesta `nav::enter_target`, que es
                // la función COMPARTIDA que usan las dos superficies. Aquí
                // había un `selected().filter(kind == Dir)` escrito a mano —o
                // sea la regla de la ventana— y por eso esta comparación no
                // podía fallar sobre un `.zip` o un enlace: medía el arnés y
                // no el producto. Es la avería que la cabecera de este fichero
                // describe, y ya se puede quitar.
                let destino = if pane.is_parent_row(pane.cursor()) {
                    pane.parent_target().cloned()
                } else {
                    pane.selected().and_then(norte_frontend::nav::enter_target)
                };
                let Some(destino) = destino else {
                    salida.push(foto_primitivas(&pane));
                    continue;
                };
                navega(&mut pane, &mut historial, &destino, Trail::Record, &arbol);
            }
            Paso::Subir => {
                let actual = pane.dir().clone();
                let Some(padre) = actual.parent() else {
                    salida.push(foto_primitivas(&pane));
                    continue;
                };
                pane.set_pending_focus(actual);
                navega(&mut pane, &mut historial, &padre, Trail::Record, &arbol);
            }
            Paso::Atras | Paso::Adelante => {
                let actual = pane.dir().clone();
                let destino = if matches!(paso, Paso::Atras) {
                    historial.step_back(actual)
                } else {
                    historial.step_forward(actual)
                };
                let Some(destino) = destino else {
                    salida.push(foto_primitivas(&pane));
                    continue;
                };
                navega(
                    &mut pane,
                    &mut historial,
                    &destino,
                    Trail::Replay(if matches!(paso, Paso::Atras) {
                        norte_frontend::nav::TrailStep::Back
                    } else {
                        norte_frontend::nav::TrailStep::Forward
                    }),
                    &arbol,
                );
            }
        }
        salida.push(foto_primitivas(&pane));
    }
    salida
}

/// El mismo escenario, contra el TERMINAL, por sus propias decisiones.
///
/// Ésta es la pata que faltaba (ADR 0097, D1). Las otras dos comparan el host
/// contra las primitivas, y el arnés de las primitivas está escrito con las
/// reglas del host —su `Entrar` era `selected().filter(|e| e.kind == Dir)`,
/// que es lo que hace la ventana y no lo que hace el terminal—, así que esa
/// comparación no podía fallar por construcción.
///
/// Aquí los pasos pasan por las funciones de decisión del TUI: `enter_action`
/// para Enter y `nav_enter_target` por debajo, que es donde el terminal
/// decide que un `.zip` se navega y un symlink se sigue.
fn via_tui(pasos: &[Paso], fila_de_subir: bool) -> Vec<Semantico> {
    use norte_tui::app::{App, Pane};

    let arbol = arbol_de_prueba();
    let inicio = VPath::parse("mem:///casa").expect("vpath");
    let mut app = App::new(
        Pane::new(inicio.clone(), entradas(&arbol, &inicio)),
        Pane::new(inicio.clone(), Vec::new()),
    );
    app.set_parent_row(fila_de_subir);
    app.set_focus(0);
    let mut salida = vec![foto_tui(&app)];

    for paso in pasos {
        match paso {
            Paso::Cursor(delta) => {
                for _ in 0..delta.unsigned_abs() {
                    if *delta < 0 {
                        app.focused_mut().move_up(1);
                    } else {
                        app.focused_mut().move_down(1);
                    }
                }
            }
            Paso::CursorA(nombre) => {
                let filas = app.focused().entries().len();
                app.focused_mut().move_up(filas);
                for _ in 0..filas {
                    if foto_tui(&app).nombres.get(app.focused().cursor())
                        == Some(&(*nombre).to_owned())
                    {
                        break;
                    }
                    app.focused_mut().move_down(1);
                }
            }
            Paso::Marcar => app.focused_mut().toggle_mark(),
            Paso::Entrar => {
                // La decisión del TERMINAL, no una copia de ella.
                match norte_tui::gestures::enter_action(&app) {
                    norte_tui::gestures::EnterAction::Cd(dir) => cd_tui(&mut app, &dir, &arbol),
                    norte_tui::gestures::EnterAction::Up(padre) => {
                        let hijo = app.focused().dir().clone();
                        app.focused_mut().set_pending_focus(hijo);
                        cd_tui(&mut app, &padre, &arbol);
                    }
                    // Abrir fuera o ver no mueve el listado: el escenario
                    // observa el listado, así que esto es un paso quieto.
                    _ => {}
                }
            }
            Paso::Subir => {
                let actual = app.focused().dir().clone();
                let Some(padre) = actual.parent() else {
                    salida.push(foto_tui(&app));
                    continue;
                };
                app.focused_mut().set_pending_focus(actual);
                cd_tui(&mut app, &padre, &arbol);
            }
            Paso::Atras | Paso::Adelante => {
                let actual = app.focused().dir().clone();
                let slot = app.panes.slot_of(app.focus());
                let destino = {
                    let h = app.history.for_slot_mut(slot);
                    if matches!(paso, Paso::Atras) {
                        h.step_back(actual)
                    } else {
                        h.step_forward(actual)
                    }
                };
                let Some(destino) = destino else {
                    salida.push(foto_tui(&app));
                    continue;
                };
                let filas = entradas(&arbol, &destino);
                // `begin_listing` es el cd de VERDAD del terminal: graba el
                // cursor del dir viejo antes de reemplazar el listado.
                app.focused_mut().begin_listing(destino, filas, false, None);
            }
        }
        salida.push(foto_tui(&app));
    }
    salida
}

/// Un `cd` del terminal: registrar el paso, recordar el cursor, listar.
fn cd_tui(app: &mut norte_tui::app::App, destino: &VPath, arbol: &Falso) {
    let anterior = app.focused().dir().clone();
    let slot = app.panes.slot_of(app.focus());
    if anterior != *destino {
        app.history.for_slot_mut(slot).record(anterior);
    }
    let filas = entradas(arbol, destino);
    app.focused_mut()
        .begin_listing(destino.clone(), filas, false, None);
}

/// La misma foto semántica, leída del terminal.
fn foto_tui(app: &norte_tui::app::App) -> Semantico {
    let pane = app.focused();
    Semantico {
        dir: norte_frontend::path_display(pane.dir()).0,
        cursor: pane.cursor(),
        marcas: pane.marks_len(),
        nombres: pane
            .entries()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                if pane.is_parent_row(i) {
                    return "..".to_owned();
                }
                norte_frontend::display_name(
                    e.path
                        .file_name()
                        .map_or(&[][..], norte_proto::Segment::as_bytes),
                )
                .0
            })
            .collect(),
    }
}

/// El mismo ritual de navegación que el host: registrar el paso si no es el
/// rastro reproduciéndose, recordar el cursor, y listar.
fn navega(
    pane: &mut PaneState,
    historial: &mut History,
    destino: &VPath,
    trail: Trail,
    arbol: &Falso,
) {
    let anterior = pane.dir().clone();
    if anterior != *destino && trail == Trail::Record {
        historial.record(anterior);
    }
    pane.remember_cursor();
    pane.set_listing(destino.clone(), entradas(arbol, destino));
}

fn entradas(arbol: &Falso, dir: &VPath) -> Vec<Entry> {
    arbol.entradas_de(dir)
}

fn foto_primitivas(pane: &PaneState) -> Semantico {
    Semantico {
        dir: norte_frontend::path_display(pane.dir()).0,
        cursor: pane.cursor(),
        marcas: pane.marks_len(),
        nombres: pane
            .entries()
            .iter()
            .enumerate()
            .map(|(i, e)| {
                // La fila `..` se pinta `..` y no con el nombre del padre —el
                // suyo es la ruta del padre, cuyo `file_name` en la raíz ni
                // siquiera existe—. Es lo que hacen los dos renderers, y esta
                // vía tiene que pintar igual o la comparación mide el arnés.
                if pane.is_parent_row(i) {
                    return "..".to_owned();
                }
                norte_frontend::display_name(
                    e.path
                        .file_name()
                        .map_or(&[][..], norte_proto::Segment::as_bytes),
                )
                .0
            })
            .collect(),
    }
}

/// El mismo escenario, contra el host, por sus acciones y sus fotos.
async fn via_host(pasos: &[Paso], fila_de_subir: bool) -> Vec<Semantico> {
    let backend = Arc::new(arbol_de_prueba());
    let (host, primera) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        initial_dir_pedido: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        // Cada escenario se corre DOS veces, con la fila `..` apagada y
        // encendida, y las dos vías tienen que coincidir en las dos. Correrlo
        // solo apagada probaba el único estado en el que nadie arranca: de
        // fábrica la fila está puesta, y el cursor nace justo encima de ella.
        settings: {
            let mut cfg = norte_ui_host::ajustes_por_defecto();
            cfg.common.ui_parent_entry = Some(fila_de_subir);
            cfg
        },
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = host.subscribe();
    let mut salida = vec![foto_host(&primera)];
    // La generación VIVA del listado. Una acción de fila la lleva porque sin
    // ella la clave es un índice, y este arnés navega entre pasos: el índice
    // de la pantalla anterior nombraría otro fichero.
    let mut epoca = listado_de(&primera).generation;

    for paso in pasos {
        let (accion, navega) = match paso {
            Paso::Cursor(delta) => (
                UiAction::MoveCursor {
                    slot_id: 1,
                    delta: *delta,
                },
                false,
            ),
            Paso::CursorA(nombre) => {
                // De una sola acción: esta superficie mueve el cursor por
                // DELTA, así que la fila buscada se convierte en uno. Lo que
                // se compara es dónde acaba, no cuántas teclas costó.
                let actual = salida.last().expect("hay foto");
                let destino = actual
                    .nombres
                    .iter()
                    .position(|n| n == nombre)
                    .unwrap_or(actual.cursor);
                let delta =
                    i64::try_from(destino).unwrap_or(0) - i64::try_from(actual.cursor).unwrap_or(0);
                (UiAction::MoveCursor { slot_id: 1, delta }, false)
            }
            Paso::Marcar => {
                let actual = salida.last().expect("hay foto");
                (
                    UiAction::ToggleMark {
                        slot_id: 1,
                        key: norte_ui_host::RowKey(u64::try_from(actual.cursor).unwrap_or(0)),
                        generation: epoca,
                    },
                    false,
                )
            }
            Paso::Entrar => {
                let actual = salida.last().expect("hay foto");
                (
                    UiAction::Activate {
                        slot_id: 1,
                        key: norte_ui_host::RowKey(u64::try_from(actual.cursor).unwrap_or(0)),
                        generation: epoca,
                    },
                    true,
                )
            }
            Paso::Subir => (UiAction::Parent { slot_id: 1 }, true),
            Paso::Atras => (
                UiAction::History {
                    slot_id: 1,
                    back: true,
                },
                true,
            ),
            Paso::Adelante => (
                UiAction::History {
                    slot_id: 1,
                    back: false,
                },
                true,
            ),
        };
        let ack = host.dispatch(accion).await.expect("host vivo");
        // Una navegación que no se puede hacer (raíz, rastro agotado, fila
        // que no es directorio) deja la pantalla como estaba: es el MISMO
        // desenlace que en las primitivas.
        // Sin adivinar si va a llegar una foto: se espera un momento por si
        // la pantalla se mueve sola —un `cd` la mueve— y, si no se mueve, se
        // pide.
        //
        // Antes se deducía del acuse (`navega && Applied`), y eso era el
        // arnés encodificando una regla del producto: `Activate` sobre un
        // fichero contesta `Applied` porque LO ABRE FUERA, así que el
        // escenario se quedaba esperando un listado que nunca iba a existir.
        // Deducirlo es además justo lo que este fichero no puede hacer: si
        // supiera qué navega, no estaría midiendo si las dos superficies
        // están de acuerdo en qué navega.
        let _ = (navega, &ack);
        let foto = match espera_foto_opcional(&mut sub).await {
            Some(f) => f,
            None => pide_foto(&host, &mut sub).await,
        };
        epoca = listado_de(&foto).generation;
        salida.push(foto_host(&foto));
    }
    salida
}

/// Una foto que llegue SOLA, o `None` si la pantalla no se movió.
///
/// El plazo es el presupuesto de FALLO: en el camino verde —un `cd`— la foto
/// ya está esperando, y en el que no se mueve nada este plazo se gasta entero
/// una vez por paso.
async fn espera_foto_opcional(sub: &mut UiSubscription) -> Option<ViewSnapshot> {
    for _ in 0..20 {
        let siguiente =
            tokio::time::timeout(std::time::Duration::from_millis(100), sub.recv()).await;
        let Ok(recibido) = siguiente else {
            return None;
        };
        if let Update::Message(m) = recibido.expect("el host sigue vivo")
            && let UiUpdate::Snapshot(s) = m.payload
        {
            return Some(*s);
        }
    }
    None
}

async fn espera_foto(sub: &mut UiSubscription) -> ViewSnapshot {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(std::time::Duration::from_millis(500), sub.recv())
            .await
            .expect("una foto, no un cuelgue")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Snapshot(s) = m.payload
        {
            return *s;
        }
    }
    panic!("no llegó ninguna foto");
}

async fn pide_foto(host: &UiHost, sub: &mut UiSubscription) -> ViewSnapshot {
    host.dispatch(UiAction::Resync).await.expect("host vivo");
    espera_foto(sub).await
}

/// El listado de una foto.
fn listado_de(snap: &ViewSnapshot) -> &norte_ui_host::dto::BrowserSlotView {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("hay listado")
    else {
        unreachable!("filtrado arriba")
    };
    b
}

fn foto_host(snap: &ViewSnapshot) -> Semantico {
    let SlotView::Browser(b) = snap
        .slots
        .iter()
        .find(|s| matches!(s, SlotView::Browser(_)))
        .expect("hay listado")
    else {
        unreachable!("filtrado arriba")
    };
    Semantico {
        dir: b.path_display.clone(),
        cursor: b.cursor.map_or(0, |k| usize::try_from(k.0).unwrap_or(0)),
        marcas: usize::try_from(b.marks).unwrap_or(0),
        nombres: b.rows.iter().map(|r| r.display_name.clone()).collect(),
    }
}

/// Corre un escenario por las dos vías y compara paso a paso.
async fn compara(nombre: &str, pasos: &[Paso]) {
    // Las dos configuraciones de la fila `..`. La encendida es la de fábrica
    // y la que ve cualquiera que abra norte; la apagada se sigue corriendo
    // porque es una opción de verdad y su listado tiene otros índices.
    for fila_de_subir in [false, true] {
        let etiqueta = if fila_de_subir {
            "con fila `..`"
        } else {
            "sin fila `..`"
        };
        let esperado = via_primitivas(pasos, fila_de_subir);
        let obtenido = via_host(pasos, fila_de_subir).await;
        assert_eq!(
            esperado.len(),
            obtenido.len(),
            "[{nombre}, {etiqueta}] distinto número de pasos observados"
        );
        for (i, (a, b)) in esperado.iter().zip(obtenido.iter()).enumerate() {
            assert_eq!(
                a, b,
                "[{nombre}, {etiqueta}] paso {i}: el host y las primitivas divergen"
            );
        }

        // Y la comparación que de verdad guarda algo: los DOS frontends, uno
        // contra otro. Las de arriba miden al host contra un arnés escrito
        // con las reglas del host.
        let terminal = via_tui(pasos, fila_de_subir);
        assert_eq!(
            terminal.len(),
            obtenido.len(),
            "[{nombre}, {etiqueta}] el terminal y la ventana observan distinto número de pasos"
        );
        for (i, (a, b)) in terminal.iter().zip(obtenido.iter()).enumerate() {
            assert_eq!(
                a, b,
                "[{nombre}, {etiqueta}] paso {i}: el TERMINAL y la VENTANA divergen"
            );
        }
    }
}

/// Listar, moverse y marcar.
#[tokio::test]
async fn listar_moverse_marcar() {
    compara(
        "listar → mover → marcar",
        &[Paso::Cursor(1), Paso::Marcar, Paso::Cursor(1), Paso::Marcar],
    )
    .await;
}

/// El cursor topa en los extremos igual en las dos superficies.
#[tokio::test]
async fn el_cursor_topa_igual() {
    compara(
        "cursor a los extremos",
        &[Paso::Cursor(-5), Paso::Cursor(99), Paso::Cursor(1)],
    )
    .await;
}

/// Entrar, volver, avanzar y subir: el rastro y la memoria del cursor se
/// comportan igual.
#[tokio::test]
async fn entrar_atras_adelante_subir() {
    compara(
        "abrir dir → atrás → adelante → subir",
        &[
            Paso::Entrar,
            Paso::Atras,
            Paso::Adelante,
            Paso::Subir,
            Paso::Atras,
        ],
    )
    .await;
}

/// `Enter` sobre un COMPRIMIDO entra en él, en las dos superficies.
///
/// La divergencia número uno del inventario, y la que este arnés no podía
/// tocar: su árbol solo tenía directorios y ficheros, así que el paso
/// `Entrar` nunca se encontraba con nada sobre lo que las dos pudieran
/// contestar distinto. El terminal navegaba al `zip+…!/` y la ventana se lo
/// daba a `xdg-open`, y esta comparación pasaba verde por debajo.
#[tokio::test]
async fn entrar_en_un_comprimido_es_lo_mismo_en_las_dos() {
    compara(
        "cursor al zip → entrar → atrás",
        &[Paso::CursorA("cosas.zip"), Paso::Entrar, Paso::Atras],
    )
    .await;
}

/// Y sobre un ENLACE, igual: se sigue sin resolver a dónde apunta.
///
/// Que el provider liste o falle es cosa suya; lo que se compara es que las
/// dos superficies hagan la MISMA pregunta. Aquí el enlace lleva a un
/// directorio que sí se lista, que es el caso en el que un desacuerdo se ve.
#[tokio::test]
async fn entrar_en_un_enlace_es_lo_mismo_en_las_dos() {
    compara(
        "cursor al enlace → entrar → subir",
        &[Paso::CursorA("atajo"), Paso::Entrar, Paso::Subir],
    )
    .await;
}

/// Y sobre un FICHERO corriente, `Enter` no navega en ninguna de las dos.
///
/// La otra mitad del contrato: si `enter_target` se volviera permisivo, los
/// dos tests de arriba seguirían verdes y este se pondría rojo.
#[tokio::test]
async fn entrar_en_un_fichero_no_navega_en_ninguna() {
    compara(
        "cursor a un fichero → entrar",
        &[Paso::CursorA("notas.txt"), Paso::Entrar],
    )
    .await;
}

/// Un escenario que mezcla marcas y navegación: las marcas NO sobreviven a un
/// listado nuevo, y eso también tiene que coincidir.
#[tokio::test]
async fn las_marcas_no_sobreviven_a_un_cd() {
    compara(
        "marcar → entrar → volver",
        &[Paso::Marcar, Paso::Entrar, Paso::Atras],
    )
    .await;
}
