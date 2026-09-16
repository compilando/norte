use super::*;

// ---------------------------------------------------------------------------
// La ayuda (fase 4, tarea 4.4).
// ---------------------------------------------------------------------------

/// Espera la siguiente actualización que traiga la ayuda.
pub(super) async fn siguiente_ayuda(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::HelpView> {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Help { help } = c {
                    return help.clone();
                }
            }
        }
    }
    panic!("no llegó ninguna actualización con ayuda");
}

/// Abre la ayuda y devuelve lo que se pintaría.
pub(super) async fn abrir_ayuda(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::HelpView {
    h.dispatch(tecla("F1")).await.expect("host vivo");
    siguiente_ayuda(sub).await.expect("la ayuda abre")
}

/// `F1` abre la ayuda sobre la página del CONTEXTO donde está el lector, con
/// su prosa ya en bloques y sin una sola marca sin resolver.
#[tokio::test]
async fn f1_abre_la_ayuda_del_contexto_y_su_prosa_llega_en_bloques() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    assert_eq!(
        ayuda.topic_id, "panes",
        "abre la página del CONTEXTO (el listado), no el índice"
    );
    assert!(!ayuda.title.is_empty(), "la página tiene título");
    assert!(!ayuda.blocks.is_empty(), "y cuerpo");
    assert!(!ayuda.sidebar.is_empty(), "y la lateral enumera lo que hay");
    assert!(
        !ayuda.can_back,
        "la página del contexto es la RAÍZ: `⌫` cierra, no vuelve a un índice \
         donde el lector no estuvo"
    );
    // Ni una marca viva sin resolver, ni una clave Fluent cruda: las dos
    // cosas son texto que el lector no debería ver jamás.
    let texto = format!("{:?}", ayuda.blocks);
    assert!(!texto.contains("{{cmd:"), "una marca sin resolver: {texto}");
    assert!(!texto.contains("[["), "un enlace sin resolver: {texto}");
    assert!(!texto.contains("help-cmd-"), "una clave Fluent cruda");
    // Una cabecera de grupo llega TRADUCIDA, no como su tag.
    let grupos: Vec<&norte_ui_host::dto::HelpSidebarRowView> = ayuda
        .sidebar
        .iter()
        .filter(|r| matches!(r, norte_ui_host::dto::HelpSidebarRowView::Group { .. }))
        .collect();
    assert!(!grupos.is_empty(), "hay cabeceras de grupo");
    for g in grupos {
        let norte_ui_host::dto::HelpSidebarRowView::Group { label } = g else {
            unreachable!("filtrado arriba")
        };
        assert!(!label.starts_with("help-group-"), "sin traducir: {label}");
    }
}

/// La hoja de teclado se GENERA del mapa efectivo: un rebind la cambia, y una
/// tecla que este frontend no ejecuta sale apagada y con su motivo.
#[tokio::test]
async fn la_hoja_de_teclado_sale_del_keymap_efectivo() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    // La página de teclado es la última de la lateral (grupo de una sola
    // fila): se llega con el cursor, como llegaría el lector.
    let ultima = ayuda.sidebar.len() - 1;
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(ultima).expect("cabe"),
    })
    .await
    .expect("host vivo");
    let teclas = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_eq!(teclas.topic_id, "keys", "se abrió la página de teclado");

    let filas: Vec<&norte_ui_host::dto::HelpKeyRowView> = teclas
        .blocks
        .iter()
        .filter_map(|b| match b {
            norte_ui_host::dto::HelpBlockView::Keys { rows } => Some(rows),
            _ => None,
        })
        .flatten()
        .collect();
    assert!(!filas.is_empty(), "la hoja tiene filas");
    assert!(
        filas.iter().any(|r| r.chord == "F5"),
        "y las escribe como las escribe la documentación: {:?}",
        filas.iter().map(|r| &r.chord).collect::<Vec<_>>()
    );
    assert!(
        filas.iter().all(|r| !r.label.starts_with("help-cmd-")),
        "ninguna fila pinta una clave Fluent"
    );
    // Hubo filas apagadas mientras `app.quit` no era de la ventana: era el
    // último comando que el preset ortodoxo ata y esta ventana no hacía.
    // Ya no queda ninguno, así que lo que se fija es la REGLA: si alguna
    // fila viene apagada, dice por qué — atenuar sin decirlo deja al lector
    // adivinando si la app está rota.
    let apagadas: Vec<&&norte_ui_host::dto::HelpKeyRowView> =
        filas.iter().filter(|r| !r.enabled).collect();
    assert!(
        apagadas.iter().all(|r| !r.reason.is_empty()),
        "cada fila apagada dice POR QUÉ"
    );
    assert!(
        filas.iter().any(|r| r.chord == "F10" && r.enabled),
        "y salir, que estuvo apagada en esta ventana, ya no lo está: {:?}",
        filas
            .iter()
            .filter(|r| r.chord == "F10")
            .map(|r| (&r.label, r.enabled))
            .collect::<Vec<_>>()
    );
}

/// Una fila ejecutable de un comando que este frontend NO implementa se
/// ofrece apagada y con su motivo, en vez de prometer un `enter` que
/// contestaría «aquí no».
#[tokio::test]
async fn una_fila_que_esta_ventana_no_ejecuta_llega_apagada() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut ayuda = abrir_ayuda(&h, &mut sub).await;

    // Se recorre la lateral hasta dar con una página que documente comandos.
    for row in 0..ayuda.sidebar.len() {
        if ayuda.actions.iter().any(|a| !a.opens_topic) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic {
            row: u32::try_from(row).expect("cabe"),
        })
        .await
        .expect("host vivo");
        ayuda = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    let corribles: Vec<&norte_ui_host::dto::HelpActionView> =
        ayuda.actions.iter().filter(|a| !a.opens_topic).collect();
    assert!(
        !corribles.is_empty(),
        "alguna página del corpus documenta comandos"
    );
    for a in corribles {
        assert!(!a.label.is_empty(), "toda fila se llama de algo");
        assert_eq!(
            a.enabled,
            a.reason.is_empty(),
            "una fila apagada dice por qué, y una viva no inventa motivo: {a:?}"
        );
    }
}

/// Activar una fila ejecutable cierra la ayuda Y corre el comando — por el
/// MISMO camino que una tecla, que es lo que hace que la ayuda sea otra
/// puerta al catálogo y no un segundo despachador.
#[tokio::test]
async fn activar_en_la_ayuda_cierra_y_ejecuta_por_el_mismo_camino() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    // La página de marcado documenta `mark.toggle`, que esta ventana SÍ
    // ejecuta: se llega a ella por la lateral, como llegaría el lector.
    let mut pagina = ayuda;
    for row in 0..40 {
        if pagina.topic_id == "selection" {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    assert_eq!(pagina.topic_id, "selection", "la página de marcado existe");
    let i = pagina
        .actions
        .iter()
        .position(|a| !a.opens_topic && a.enabled)
        .expect("alguna de sus filas la ejecuta esta ventana");

    h.dispatch(UiAction::HelpActivate {
        index: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "correr cierra la ayuda, y el cierre viaja como parche"
    );

    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.help.is_none(), "y sigue cerrada en la foto");
    assert!(
        listado(&foto).rows.iter().any(|r| r.marked),
        "y el comando de marcado se ejecutó de verdad"
    );
}

/// Seguir un enlace de «ver también» abre la otra página y DEJA la ayuda
/// abierta: es navegación, no una acción sobre el listado.
#[tokio::test]
async fn seguir_un_enlace_abre_la_otra_pagina_y_deja_volver() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut pagina = abrir_ayuda(&h, &mut sub).await;

    for row in 0..40 {
        if pagina.actions.iter().any(|a| a.opens_topic) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    let i = pagina
        .actions
        .iter()
        .position(|a| a.opens_topic)
        .expect("alguna página enlaza a otra");
    let antes = pagina.topic_id.clone();

    h.dispatch(UiAction::HelpActivate {
        index: u32::try_from(i).expect("cabe"),
    })
    .await
    .expect("host vivo");
    let seguida = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_ne!(seguida.topic_id, antes, "cambió de página");
    assert!(seguida.can_back, "y hay a dónde volver");

    h.dispatch(tecla("Backspace")).await.expect("host vivo");
    let vuelta = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_eq!(vuelta.topic_id, antes, "`⌫` vuelve por donde vino");
}

/// `esc` la cierra; `/` abre el filtro y entonces las teclas de texto son
/// suyas.
#[tokio::test]
async fn la_barra_filtra_y_escape_cierra() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;
    assert!(!ayuda.filtering, "arranca sin filtro");

    h.dispatch(tecla("/")).await.expect("host vivo");
    let filtrando = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert!(filtrando.filtering, "`/` abre el filtro");

    h.dispatch(tecla("c")).await.expect("host vivo");
    let tecleada = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert_eq!(tecleada.filter, "c", "y la letra la escribe el filtro");

    // El primer `esc` deja de filtrar; el segundo cierra.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    let sin_filtro = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    assert!(!sin_filtro.filtering, "el primer esc abandona el filtro");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "el segundo cierra la ayuda"
    );
}

/// Con la ayuda abierta, una tecla del listado NO se cuela: la pantalla es
/// suya, como la del visor.
#[tokio::test]
async fn con_la_ayuda_abierta_el_listado_no_se_mueve() {
    let (h, snap) = host_arbol(arbol()).await;
    let antes = listado(&snap).cursor;
    let mut sub = h.subscribe();
    let _ = abrir_ayuda(&h, &mut sub).await;

    // `j` en el preset baja el cursor; con la ayuda abierta no es del
    // listado, y sin filtro abierto tampoco teclea nada.
    h.dispatch(tecla("j")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, antes, "el listado no se movió");
    assert!(foto.help.is_some(), "y la ayuda sigue abierta en la foto");
}

/// `Ctrl+P` sale de la ayuda a la paleta, y los DOS cambios viajan en el
/// mismo parche: un renderer que solo recibiera el de la paleta seguiría
/// pintando la ayuda debajo.
#[tokio::test]
async fn ctrl_p_cambia_la_ayuda_por_la_paleta_en_un_solo_parche() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let _ = abrir_ayuda(&h, &mut sub).await;

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");

    let mut vio_cierre = false;
    let mut vio_paleta = false;
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización antes del plazo")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                match c {
                    norte_ui_host::dto::ViewChange::Help { help } => vio_cierre = help.is_none(),
                    norte_ui_host::dto::ViewChange::Palette { palette } => {
                        vio_paleta = palette.is_some();
                    }
                    _ => {}
                }
            }
            if vio_cierre && vio_paleta {
                return;
            }
        }
    }
    panic!("el relevo no viajó entero: cierre={vio_cierre}, paleta={vio_paleta}");
}

// ---------------------------------------------------------------------------
// Las páginas de extensión de la ayuda (H3e sobre el host gráfico).
// ---------------------------------------------------------------------------

/// Un plugin del catálogo, con lo mínimo que la ayuda mira.
pub(super) fn extension(id: &str, name: &str, has_help: bool) -> norte_proto::methods::PluginInfo {
    norte_proto::methods::PluginInfo {
        id: id.to_owned(),
        name: name.to_owned(),
        publisher: "ACME".to_owned(),
        version: "1.0.0".to_owned(),
        category: "previewer".to_owned(),
        capabilities: Vec::new(),
        approved: true,
        enabled: true,
        description: None,
        commands: Vec::new(),
        columns: Vec::new(),
        panels: Vec::new(),
        has_help,
        // El ancla que el core manda (#282): la ventana la devuelve al
        // confirmar, y sin ella en el doble el hilo entero no se ejercitaría.
        manifest_digest: Some(format!("digest-de-{id}")),
    }
}

/// Un árbol con catálogo de extensiones.
pub(super) fn arbol_con_plugins(
    plugins: Vec<norte_proto::methods::PluginInfo>,
    paginas: &[(&str, &str)],
) -> Arc<Falso> {
    let base = arbol();
    let mut f = Falso {
        plugins: plugins.into(),
        paginas: paginas
            .iter()
            .map(|(id, md)| ((*id).to_owned(), (*md).to_owned()))
            .collect(),
        ..Falso::default()
    };
    f.arbol.clone_from(&base.arbol);
    Arc::new(f)
}

/// Espera a que la lateral tenga una fila cuyo título contenga `aguja`.
pub(super) async fn ayuda_con_fila(
    sub: &mut norte_ui_host::UiSubscription,
    aguja: &str,
) -> norte_ui_host::dto::HelpView {
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(sub).await else {
            continue;
        };
        if v.sidebar.iter().any(|r| match r {
            norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } => title.contains(aguja),
            norte_ui_host::dto::HelpSidebarRowView::Group { .. } => false,
        }) {
            return v;
        }
    }
    panic!("la lateral nunca trajo una fila con {aguja:?}");
}

/// Una extensión con página aparece en la lateral, y abrirla PIDE su página y
/// la instala con su línea de procedencia.
#[tokio::test]
async fn una_extension_con_pagina_se_lee_desde_la_ayuda() {
    let backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true)],
        &[("acme.ftp", "Conecta con un servidor FTP.")],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F1")).await.expect("host vivo");
    let ayuda = ayuda_con_fila(&mut sub, "FTP de ACME").await;

    let fila = ayuda
        .sidebar
        .iter()
        .position(|r| {
            matches!(r, norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. }
                if title.contains("FTP de ACME"))
        })
        .expect("la fila está");
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(fila).expect("cabe"),
    })
    .await
    .expect("host vivo");

    // La página llega ASÍNCRONA: primero la página vacía con su nombre, y
    // luego el cuerpo cuando el daemon contesta.
    let mut pagina = None;
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(&mut sub).await else {
            continue;
        };
        if v.topic_id == "acme.ftp" && !v.blocks.is_empty() {
            pagina = Some(v);
            break;
        }
    }
    let pagina = pagina.expect("la página del plugin se instala");
    let texto = format!("{:?}", pagina.blocks);
    assert!(texto.contains("Conecta con un servidor FTP"), "{texto}");
    // Y lleva su procedencia: una página de tercero SIEMPRE la lleva, o
    // tendría la misma forma que una del binario.
    let badge = pagina.badge.expect("una página de plugin lleva insignia");
    assert!(badge.contains("ACME"), "dice quién la publica: {badge}");

    assert_eq!(
        backend
            .paginas_pedidas
            .lock()
            .expect("mutex")
            .as_slice()
            .iter()
            .filter(|i| i.as_str() == "acme.ftp")
            .count(),
        1,
        "la página se pide UNA vez por apertura"
    );
}

/// Un id que no es reverse-DNS válido se DESCARTA en la entrada: ni fila, ni
/// petición al wire. Enmascararlo no valdría — no es inyectivo, así que dos
/// plugins distintos caerían en la misma fila.
#[tokio::test]
async fn un_id_de_extension_invalido_ni_se_pinta_ni_llega_al_wire() {
    let backend = arbol_con_plugins(
        vec![
            extension("acme.\u{202e}ftp", "Malicioso", true),
            extension("sinpunto", "Tampoco", true),
        ],
        &[],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;

    // Se le dan varias vueltas al bucle: si llegara una fila, llegaría aquí.
    for _ in 0..4 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let foto = siguiente_foto(&mut sub).await;
        let lateral = foto.help.map_or_else(Vec::new, |v| v.sidebar);
        for r in &lateral {
            if let norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } = r {
                assert!(!title.contains("Malicioso"), "entró un id inválido");
                assert!(!title.contains("Tampoco"), "entró un id sin punto");
            }
        }
    }
    assert!(
        backend.paginas_pedidas.lock().expect("mutex").is_empty(),
        "un id inválido jamás se manda al wire"
    );
    let _ = ayuda;
}

/// Un `help.md` hostil se PARSEA antes de pintarse: lo que cruza son bloques,
/// y ni un peligro de terminal viaja dentro de ellos.
#[tokio::test]
async fn una_pagina_hostil_cruza_ya_parseada_y_enmascarada() {
    let backend = arbol_con_plugins(
        vec![extension("acme.ftp", "FTP de ACME", true)],
        &[(
            "acme.ftp",
            "Texto \u{202e}con override\u{7} y un pitido.\n\n{{cmd:pane.copy}}\n",
        )],
    );
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F1")).await.expect("host vivo");
    let ayuda = ayuda_con_fila(&mut sub, "FTP de ACME").await;
    let fila = ayuda
        .sidebar
        .iter()
        .position(|r| {
            matches!(r, norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. }
                if title.contains("FTP de ACME"))
        })
        .expect("la fila está");
    h.dispatch(UiAction::HelpSelectTopic {
        row: u32::try_from(fila).expect("cabe"),
    })
    .await
    .expect("host vivo");

    let mut pagina = None;
    for _ in 0..20 {
        let Some(v) = siguiente_ayuda(&mut sub).await else {
            continue;
        };
        if v.topic_id == "acme.ftp" && !v.blocks.is_empty() {
            pagina = Some(v);
            break;
        }
    }
    let pagina = pagina.expect("la página se instala");
    let texto = format!("{:?}", pagina.blocks);
    assert!(
        !texto.contains('\u{202e}'),
        "un override bidi cruzó: {texto}"
    );
    assert!(!texto.contains('\u{7}'), "un control cruzó: {texto}");
    // Y la marca de un comando de OTRO —el binario— no se resuelve en una
    // página de plugin: un tercero no toma prestado el aviso del host.
    assert!(
        !texto.contains("F5"),
        "una página de plugin no resuelve marcas ajenas: {texto}"
    );
}

// ---------------------------------------------------------------------------
// Lo que encontraron las revisiones de la 4.4.
// ---------------------------------------------------------------------------

/// `F1` con el VISOR abierto abre la ayuda Y se queda las teclas.
///
/// Antes no: el visor iba primero en el enrutado, así que la ayuda se
/// construía, viajaba, y ninguna tecla llegaba a ella — ni la que la cierra.
/// Encima, en el DOM la ayuda estaba ANTES del visor, cuyo fondo es opaco, o
/// sea que ni se veía. Una ventana con un overlay abierto que no responde a
/// nada es lo más parecido a estar colgada.
#[tokio::test]
async fn con_el_visor_abierto_la_ayuda_se_queda_las_teclas() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/notas.txt".to_owned(), b"hola".to_vec());
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.viewer.is_some() {
            break;
        }
    }

    h.dispatch(tecla("F1")).await.expect("host vivo");
    let ayuda = siguiente_ayuda(&mut sub).await.expect("la ayuda abre");
    assert_eq!(
        ayuda.topic_id, "viewer",
        "y sobre la página del visor, que es donde está el lector"
    );

    // `esc` es de la AYUDA, no del visor: la ayuda está encima.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_ayuda(&mut sub).await.is_none(),
        "esc cierra la ayuda"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.help.is_none(), "la ayuda se fue");
    assert!(foto.viewer.is_some(), "y el visor sigue donde estaba");
}

/// Sobre un diálogo que se está TECLEANDO, `F1` no abre nada.
///
/// La ayuda se queda el teclado, así que abrirla encima de un campo de texto
/// convierte el `⌫` que corrige una errata en un paso atrás de la ayuda.
#[tokio::test]
async fn la_ayuda_no_se_abre_encima_de_un_campo_de_texto() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    // F7 abre el prompt de crear directorio.
    h.dispatch(tecla("F7")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub)
            .await
            .dialogs
            .iter()
            .any(|d| d.input.is_some())
        {
            break;
        }
    }

    h.dispatch(tecla("F1")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.help.is_none(), "la ayuda no se abrió encima del campo");
    assert!(
        foto.dialogs.iter().any(|d| d.input.is_some()),
        "y el diálogo sigue esperando el nombre"
    );
}

/// Una fila de OTRA pantalla no se ofrece encendida, y su motivo lo dice.
///
/// La lista de comandos del host es plana —listado y visor juntos—, así que
/// preguntarle a secas encendía `viewer.close` con el visor cerrado, para
/// luego negarse al pulsarla. Y un verbo `dialog.*` no lo hace esta ventana
/// ni tiene por qué: lo contesta el propio diálogo con sus botones.
#[tokio::test]
async fn una_fila_de_otra_pantalla_no_se_ofrece_encendida() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut pagina = abrir_ayuda(&h, &mut sub).await;

    for row in 0..40 {
        if pagina.topic_id == "viewer" {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    assert_eq!(pagina.topic_id, "viewer", "la página del visor existe");
    // `pane.open`, `pane.edit` y `pane.edit-new` salen en esta página y NO son
    // del visor: los tres actúan sobre el LISTADO con la aplicación del
    // escritorio, así que estar vivos aquí es lo correcto (#290 hizo que
    // editar fuera lo segundo, y que crear-y-editar fuera lo tercero). Las
    // demás filas sí necesitan el visor.
    let del_listado = [
        norte_frontend::keymap::paint_chord("alt+f4"),
        norte_frontend::keymap::paint_chord("f4"),
        norte_frontend::keymap::paint_chord("shift+f4"),
    ];
    let corribles: Vec<&norte_ui_host::dto::HelpActionView> = pagina
        .actions
        .iter()
        .filter(|a| !a.opens_topic && !del_listado.contains(&a.chord))
        .collect();
    assert!(!corribles.is_empty(), "documenta comandos");
    for a in corribles {
        assert!(
            !a.enabled,
            "sin visor abierto, ninguna fila suya se puede correr: {a:?}"
        );
        assert!(!a.reason.is_empty(), "y cada una dice por qué: {a:?}");
    }
}

/// `enter` sobre una fila apagada NO la corre, y la página sigue abierta.
///
/// El renderer no le pone escuchador a una fila apagada, pero el teclado no
/// pasa por el renderer: la comprobación tiene que estar en el host o hay una
/// puerta sin cerrojo.
#[tokio::test]
async fn enter_sobre_una_fila_apagada_no_la_corre() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let mut pagina = abrir_ayuda(&h, &mut sub).await;
    for row in 0..40 {
        if pagina.actions.iter().any(|a| !a.opens_topic && !a.enabled) {
            break;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(&mut sub).await.expect("sigue abierta");
    }
    let i = pagina
        .actions
        .iter()
        .position(|a| !a.opens_topic && !a.enabled)
        .expect("alguna página documenta un comando que esta ventana no hace");

    let ack = h
        .dispatch(UiAction::HelpActivate {
            index: u32::try_from(i).expect("cabe"),
        })
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, norte_ui_host::ActionAck::Unavailable { .. }),
        "se dice que no se puede: {ack:?}"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.help.is_some(),
        "y la ayuda SIGUE abierta: la explicación está en la página"
    );
}

/// Todo contexto que este host declara tiene una página que lo reclama, en
/// los DOS idiomas.
///
/// Sin esto, renombrar una portada del corpus deja a `F1` abriendo el índice
/// en silencio y ningún test se pone rojo.
#[test]
fn todo_contexto_declarado_tiene_pagina() {
    for lang in [norte_help::Lang::En, norte_help::Lang::Es] {
        for c in norte_ui_host::controller::CONTEXTOS {
            assert!(
                norte_help::topic_for_context(lang, c).is_some(),
                "ninguna página reclama {c} en {lang:?}"
            );
        }
    }
}

/// NINGUNA cadena de la ayuda proyectada lleva un peligro de terminal, en
/// ninguna página del corpus y en los dos idiomas.
///
/// Es la invariante sobre la que descansa todo el diseño —el renderer pinta
/// lo que llega y no lo interpreta—, y no la afirmaba nada. Se barre la
/// proyección ENTERA (título, insignia, lateral, bloques, filas y motivos):
/// una cadena nueva que se olvide de sanear se cae aquí sin que nadie tenga
/// que acordarse de añadirle su aserción.
#[tokio::test]
async fn ninguna_cadena_de_la_ayuda_lleva_un_peligro_de_terminal() {
    /// Todo lo pintable de una proyección, en una sola cadena.
    fn todo(v: &norte_ui_host::dto::HelpView) -> String {
        use std::fmt::Write as _;

        let mut s = format!("{} {}", v.title, v.filter);
        if let Some(b) = &v.badge {
            s.push_str(b);
        }
        for r in &v.sidebar {
            match r {
                norte_ui_host::dto::HelpSidebarRowView::Group { label } => s.push_str(label),
                norte_ui_host::dto::HelpSidebarRowView::Topic { title, .. } => s.push_str(title),
            }
        }
        // Los bloques y las filas se barren por su `Debug`, que incluye
        // TODOS sus campos: es justamente lo que hace que una cadena nueva
        // entre en el barrido sin tocar este test.
        let _ = write!(s, "{:?}{:?}", v.blocks, v.actions);
        s
    }

    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ayuda = abrir_ayuda(&h, &mut sub).await;
    let filas = ayuda.sidebar.len();
    let mut vistas = vec![ayuda];
    for row in 0..filas {
        h.dispatch(UiAction::HelpSelectTopic {
            row: u32::try_from(row).expect("cabe"),
        })
        .await
        .expect("host vivo");
        if let Some(v) = siguiente_ayuda(&mut sub).await {
            vistas.push(v);
        }
    }
    assert!(vistas.len() > 3, "se recorrieron varias páginas");
    for v in &vistas {
        let texto = todo(v);
        // El `Debug` de un `&str` escapa los controles como `\u{...}`, así
        // que se busca sobre el texto DESESCAPADO de los campos planos y,
        // para los anidados, sobre la forma escapada — que delata igual.
        assert!(
            !texto.chars().any(norte_encoding::is_terminal_hazard),
            "peligro de terminal en la página {}: {texto:?}",
            v.topic_id
        );
        assert!(
            !texto.contains("\\u{202e}") && !texto.contains("\\u{7}"),
            "peligro escapado en la página {}",
            v.topic_id
        );
    }
}
