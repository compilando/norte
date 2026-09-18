use super::*;
use norte_ui_host::dto::OrganizeLineKind;

// ---------------------------------------------------------------------------
// Organizar un directorio desde la ventana (fase 8 del programa WOW).
// ---------------------------------------------------------------------------

/// Un hash cualquiera, en la forma que exige el protocolo.
fn hash() -> norte_proto::methods::PlanHash {
    norte_proto::methods::PlanHash::parse(&"ab".repeat(32)).expect("64 hex en minúscula")
}

/// Un doble con un plan de organizar listo y su token.
fn falso_con_arbol(moves: &[(&str, &str)], con_token: bool) -> Arc<Falso> {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        moves
            .iter()
            .map(|(from, _)| (from.as_bytes().to_vec(), false))
            .collect::<Vec<_>>(),
    );
    f.plan_organizar = Some(
        moves
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    );
    f.organizar_hash = con_token.then(hash);
    Arc::new(f)
}

/// Espera la siguiente actualización que traiga el árbol de organizar.
async fn siguiente_arbol(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::OrganizeView> {
    for _ in 0..40 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Organize { organize } = c {
                            return organize.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload
                    && s.organize.is_some()
                {
                    return s.organize.clone();
                }
            }
            Update::Lagged => panic!("sin retraso en este test"),
        }
    }
    panic!("ninguna actualización trajo el árbol");
}

/// Pide el plan por la paleta. SIN prompt de instrucción, a diferencia de
/// renombrar: lo que se pide es «mira este directorio y propón una forma».
async fn pedir_arbol(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    por_la_paleta(h, sub, "organize").await;
}

/// El árbol se abre con su recuento, y una carpeta que YA estaba no se pinta
/// como nueva: pintarlo todo como nuevo enseña un plan más espectacular de lo
/// que es y esconde que algo cae dentro de algo que el lector ya tenía.
#[tokio::test]
async fn el_arbol_distingue_lo_que_se_crea_de_lo_que_ya_estaba() {
    let mut f = Falso::default();
    // `facturas` ya existe en el directorio; `nueva` no.
    f.pon(
        "mem:///casa",
        vec![
            (b"a.pdf".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"facturas".to_vec(), true),
        ],
    );
    f.plan_organizar = Some(vec![
        ("a.pdf".to_owned(), "facturas/a.pdf".to_owned()),
        ("b.txt".to_owned(), "nueva/b.txt".to_owned()),
    ]);
    f.organizar_hash = Some(hash());
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_arbol(&h, &mut sub).await;

    let v = siguiente_arbol(&mut sub).await.expect("abre");
    let facturas = v
        .lines
        .iter()
        .find(|l| l.text.text == "facturas")
        .expect("está");
    assert_eq!(facturas.kind, OrganizeLineKind::ExistingDir, "{facturas:?}");
    let nueva = v
        .lines
        .iter()
        .find(|l| l.text.text == "nueva")
        .expect("está");
    assert_eq!(nueva.kind, OrganizeLineKind::NewDir, "{nueva:?}");
    // Y el resumen cuenta UNA carpeta nueva, no dos.
    assert!(v.summary.contains('1'), "el recuento: {}", v.summary);
}

/// Un plan SIN token no abre revisión.
///
/// Sin `plan_hash` no hay nada que canjear, así que aprobar sería un botón
/// que no puede hacer nada — y enseñar el árbol lo prometería.
#[tokio::test]
async fn un_plan_sin_token_no_abre_la_revision() {
    let backend = falso_con_arbol(&[("a.pdf", "facturas/a.pdf")], false);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_arbol(&h, &mut sub).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.organize.is_none(),
        "sin token no hay revisión: {:?}",
        foto.organize
    );
    assert!(backend.organizados.lock().expect("organizados").is_empty());
}

/// Aprobar exige haber recorrido el árbol ENTERO, y recorrerlo con el ratón
/// cuenta igual que con el teclado.
#[tokio::test]
async fn aprobar_exige_haber_llegado_al_final() {
    // Doce ficheros en la raíz: doce líneas, más que la ventana de diez.
    let moves: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.txt"), format!("b{i:02}.txt")))
        .collect();
    let refs: Vec<(&str, &str)> = moves
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_arbol(&refs, true);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_arbol(&h, &mut sub).await;
    let v = siguiente_arbol(&mut sub).await.expect("abre");
    assert!(!v.seen_all, "recién abierto no se ha leído entero");

    // Aprobar ahora NO manda nada.
    h.dispatch(UiAction::OrganizeDecide { approve: true })
        .await
        .expect("host vivo");
    asentar().await;
    assert!(
        backend.organizados.lock().expect("organizados").is_empty(),
        "sin leerlo entero no se aplica"
    );

    // Se recorre con el RATÓN hasta el final, y entonces sí.
    for _ in 0..12 {
        h.dispatch(UiAction::OrganizeScroll { down: true })
            .await
            .expect("host vivo");
    }
    asentar().await;
    h.dispatch(UiAction::OrganizeDecide { approve: true })
        .await
        .expect("host vivo");
    let mandado = hasta(&backend, "el plan aplicado", |f| {
        let v = f.organizados.lock().expect("organizados");
        (!v.is_empty()).then(|| v.len())
    })
    .await;
    assert_eq!(mandado, 1, "una sola Task para el lote entero");
    let hecho = backend.organizados.lock().expect("organizados");
    assert_eq!(hecho[0].2, hash(), "con el token que vino CON el plan");
    assert_eq!(hecho[0].1.len(), 12);
}

/// A un plugin hay que DARLE los nombres: no lista directorios (regla 9), y
/// con la lista vacía contesta —correctamente— que no mueve nada.
///
/// Este es el bug que destapó pilotar la fase 8 en un terminal de verdad: el
/// árbol nunca se abría y la barra decía «el plan no mueve nada» sobre un
/// directorio con cinco ficheros dentro.
#[tokio::test]
async fn a_un_organizer_se_le_dan_los_nombres_del_directorio() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"a.pdf".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    f.plan_organizar = Some(vec![("a.pdf".to_owned(), "pdf/a.pdf".to_owned())]);
    f.organizar_hash = Some(hash());
    let mut ext = crate::ayuda::extension("org.norte.by-extension", "By extension", false);
    ext.commands = vec![norte_proto::methods::PluginCommandInfo {
        id: "by-extension".to_owned(),
        title: "Into folders by extension".to_owned(),
        kind: norte_proto::methods::PluginCommandKind::Organizer,
    }];
    *f.plugins.lock().expect("plugins") = vec![ext];
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // Por la PALETA, que es la vía del organizer de un plugin.
    h.dispatch(tecla_mod("p", true, false))
        .await
        .expect("host vivo");
    let mut llego = false;
    for _ in 0..2_000 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let p = siguiente_foto(&mut sub).await.palette.expect("abierta");
        if p.rows.iter().any(|r| r.text.contains("Into folders")) {
            llego = true;
            break;
        }
    }
    assert!(llego, "la fila del organizer nunca llegó");
    for c in "Into folders".chars() {
        h.dispatch(tecla(&c.to_string())).await.expect("host vivo");
    }
    h.dispatch(tecla("Enter")).await.expect("host vivo");
    let pedido = hasta(&backend, "la petición al organizer", |f| {
        f.organizers_pedidos
            .lock()
            .expect("organizers")
            .first()
            .cloned()
    })
    .await;
    assert_eq!(
        pedido.2,
        vec!["a.pdf".to_owned(), "b.txt".to_owned()],
        "el operando es el directorio entero, no una lista vacía"
    );
}

/// Descartar no aplica nada y deja la revisión cerrada.
#[tokio::test]
async fn descartar_cierra_y_no_aplica_nada() {
    let backend = falso_con_arbol(&[("a.pdf", "facturas/a.pdf")], true);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_arbol(&h, &mut sub).await;
    siguiente_arbol(&mut sub).await.expect("abre");

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.organize.is_none(), "se cerró y sigue cerrada");
    assert!(backend.organizados.lock().expect("organizados").is_empty());
}

/// Los nombres del árbol los propone un tercero sobre nombres que escribió
/// cualquiera: se enmascaran y se DICE.
#[tokio::test]
async fn un_nombre_hostil_del_arbol_va_marcado() {
    // Del corpus canónico, no escrito a mano.
    let bytes = hostil("rtl_override");
    let alterado = String::from_utf8(bytes).expect("el del corpus es UTF-8");
    let destino = format!("facturas/{alterado}");
    let backend = falso_con_arbol(&[("a.pdf", destino.as_str())], true);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_arbol(&h, &mut sub).await;
    let v = siguiente_arbol(&mut sub).await.expect("abre");
    let fichero = v
        .lines
        .iter()
        .find(|l| l.kind == OrganizeLineKind::Moved)
        .expect("está");
    assert!(
        !fichero.text.text.contains('\u{202E}'),
        "enmascarado: {:?}",
        fichero.text
    );
    assert!(fichero.text.hostile, "y marcado: {:?}", fichero.text);
}

/// El productor puede REHUSAR con un motivo (#332): se dice y no se abre nada.
#[tokio::test]
async fn un_productor_que_rehusa_lo_dice_y_no_abre_nada() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"a.pdf".to_vec(), false)]);
    f.organizar_rehusa = Some("aprueba mi capacidad `location`".to_owned());
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_arbol(&h, &mut sub).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.organize.is_none(), "rehusar no abre revisión");
    let dicho = foto.status.message.unwrap_or_default();
    assert!(dicho.contains("location"), "y dice por qué: {dicho}");
}

/// En una ventana de SOLO LECTURA no se pide siquiera el plan: enseñar un
/// árbol que no se va a poder aplicar es prometer trabajo.
#[tokio::test]
async fn en_solo_lectura_ni_se_pide() {
    let backend = falso_con_arbol(&[("a.pdf", "facturas/a.pdf")], true);
    let (h, _snap) = crate::revisiones::host_solo_lectura(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_arbol(&h, &mut sub).await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.organize.is_none(), "ni se abre");
    assert!(backend.organizados.lock().expect("organizados").is_empty());
}
