//! Paridad: el host y las primitivas compartidas hacen LO MISMO.
//!
//! El host no debe reimplementar ninguna regla de presentación. Eso es fácil
//! de decir en un comentario y difícil de mantener: basta un `sort` propio,
//! un clamp de cursor a mano o un «esto es más cómodo así» para que dos
//! superficies empiecen a leerse distinto sin que nada se ponga rojo.
//!
//! Este arnés lo convierte en un test. Cada escenario se ejecuta DOS veces
//! —una contra `norte_frontend::PaneState` + `nav::History` directamente, y
//! otra contra el host por sus acciones y sus fotos— y se compara el estado
//! SEMÁNTICO paso a paso: dónde está el cursor, qué hay marcado, qué
//! directorio se ve y con qué nombres. Nunca píxeles.

use std::sync::Arc;

use norte_frontend::PaneState;
use norte_frontend::nav::{History, Trail};
use norte_proto::{Entry, EntryKind, VPath};
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
fn via_primitivas(pasos: &[Paso]) -> Vec<Semantico> {
    let arbol = arbol_de_prueba();
    let inicio = VPath::parse("mem:///casa").expect("vpath");
    let mut pane = PaneState::new(inicio.clone(), entradas(&arbol, &inicio));
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
            Paso::Marcar => pane.toggle_mark(),
            Paso::Entrar => {
                let Some(destino) = pane
                    .selected()
                    .filter(|e| e.kind == EntryKind::Dir)
                    .map(|e| e.path.clone())
                else {
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
            .map(|e| {
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
async fn via_host(pasos: &[Paso]) -> Vec<Semantico> {
    let backend = Arc::new(arbol_de_prueba());
    let (host, primera) = UiHost::start(UiHostOptions {
        backend,
        initial_dir: VPath::parse("mem:///casa").expect("vpath"),
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        // La fila `..` apagada: estos tests razonan sobre índices de
        // listado, y una fila más al principio los desplazaría todos sin
        // decir nada de lo que prueban.
        settings: {
            let mut cfg = norte_ui_host::ajustes_por_defecto();
            cfg.common.ui_parent_entry = Some(false);
            cfg
        },
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        columns: norte_ui_host::columnas_por_defecto(),
        effects: norte_ui_host::commands::Efectos::Completo,
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
        let hubo_cd = navega && matches!(ack, norte_ui_host::ActionAck::Applied { .. });
        let foto = if hubo_cd {
            espera_foto(&mut sub).await
        } else {
            pide_foto(&host, &mut sub).await
        };
        epoca = listado_de(&foto).generation;
        salida.push(foto_host(&foto));
    }
    salida
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
    let esperado = via_primitivas(pasos);
    let obtenido = via_host(pasos).await;
    assert_eq!(
        esperado.len(),
        obtenido.len(),
        "[{nombre}] distinto número de pasos observados"
    );
    for (i, (a, b)) in esperado.iter().zip(obtenido.iter()).enumerate() {
        assert_eq!(
            a, b,
            "[{nombre}] paso {i}: el host y las primitivas divergen"
        );
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
