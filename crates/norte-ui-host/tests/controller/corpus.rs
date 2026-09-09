use super::*;

// ---------------------------------------------------------------------------
// El corpus canónico contra las superficies nuevas.
// ---------------------------------------------------------------------------

/// Ninguna superficie deja pasar un peligro de terminal, y la que enmascara
/// lo DICE.
///
/// Una tabla sobre el corpus de `norte-testkit`, que es lo que faltaba: las
/// superficies de esta fase se escribieron sin que ninguna lo tocara, y todas
/// las banderas que se calculaban y se tiraban habrían salido de aquí. La
/// propiedad es un PAR: lo pintado no lleva peligro Y la marca está puesta.
/// Comprobar solo lo primero es lo que deja pasar una superficie que enmascara
/// en silencio.
///
/// TRES superficies, y se dice cuáles porque el doc de antes prometía nueve y
/// ejercitaba dos (#277): el nombre de un FAVORITO (lo escribe el usuario en
/// su `norte.toml`), la etiqueta de un VOLUMEN (la da el sistema y son bytes)
/// y el valor de un ATRIBUTO (el nombre de la entrada bajo el cursor). El
/// diálogo de APROBACIÓN tiene su propia tabla, porque sus rutas llegan del
/// daemon ya redactadas y hay que pasarlas antes por el mismo lossy.
#[tokio::test]
// Larga por TABLA, no por lógica: cada superficie es un bloque con su
// aserción y su frase, y partirla escondería cuáles se cubren.
#[expect(
    clippy::too_many_lines,
    reason = "tabla de superficies: una aserción y su frase por bloque"
)]
pub(super) async fn ninguna_superficie_enmascara_en_silencio() {
    let corpus = norte_testkit::corpus::hostile_names();
    assert!(
        corpus.len() >= 48,
        "el corpus canónico está: {}",
        corpus.len()
    );

    // Los que de verdad ALTERAN la pantalla. Un nombre largo o con NFD no se
    // enmascara —ni debe—, así que exigirle marca sería exigir una mentira.
    let alteran: Vec<&norte_testkit::corpus::HostileName> = corpus
        .iter()
        .filter(|n| norte_frontend::display_name(&n.bytes).1)
        .collect();
    assert!(
        alteran.len() >= 8,
        "el corpus trae peligros de verdad: {}",
        alteran.len()
    );

    for n in alteran {
        // El nombre de un favorito vive en un `String` del `norte.toml`, así
        // que solo puede llevar lo que sea UTF-8 válido. Convertir el resto
        // con `from_utf8_lossy` sería hacer aquí la conversión que el host
        // tiene que marcar, y el test diría que el host no la marca cuando
        // quien la hizo fue el test: es la trampa del doble lossy, que la
        // bandera ya no puede recuperar porque U+FFFD no es un peligro.
        let texto = match std::str::from_utf8(&n.bytes) {
            Ok(t) => t.to_owned(),
            Err(_) => String::new(),
        };

        // 1. El nombre de un FAVORITO: lo escribe el usuario, y la capa de
        //    proyecto es «he abierto este repo», no «doy fe de esta cadena».
        let mut cfg = ajustes_de_prueba();
        cfg.common.hotlist = vec![norte_config::HotlistItem {
            name: texto.clone(),
            target: norte_proto::VPath::parse("mem:///casa").map_err(|_| "err".to_owned()),
        }];
        let mut f = Falso::default();
        // La entrada bajo el cursor es la hostil: su nombre es el primer campo
        // de la HOJA DE ATRIBUTOS, que es la tercera superficie.
        f.pon("mem:///casa", vec![(n.bytes.clone(), false)]);
        // 2. La etiqueta de un VOLUMEN: la da el sistema y son bytes.
        f.volumenes = vec![norte_proto::methods::Volume {
            label: Some(n.bytes.clone()),
            ..volumen("mem:///casa", "ext4", false)
        }];
        let (h, snap) = UiHost::start(UiHostOptions {
            backend: Arc::new(f),
            initial_dir: dir(),
            initial_dir_pedido: false,
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox")
                .expect("preset"),
            layout: norte_frontend::layout::presets::tree("full").expect("layout"),
            viewport: (200, 60),
            settings: cfg,
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
        let mut sub = h.subscribe();

        let barra = sitios(&snap).expect("`full` coloca la barra").clone();
        if !texto.is_empty() {
            let favorito = barra
                .rows
                .iter()
                .find_map(|r| match r {
                    norte_ui_host::dto::PlaceRowView::Favorite { name, hostile, .. } => {
                        Some((name.clone(), *hostile))
                    }
                    _ => None,
                })
                .expect("el favorito está");
            sin_peligro(&favorito.0, &n.id, "el nombre de un favorito");
            assert!(
                favorito.1,
                "[{}] el favorito se enmascara y NO lo dice: {:?}",
                n.id, favorito.0
            );
        }

        // La barra lateral, cuando lleguen los volúmenes.
        for _ in 0..20 {
            h.dispatch(UiAction::Resync).await.expect("host vivo");
            let foto = siguiente_foto(&mut sub).await;
            let v = sitios(&foto).expect("colocada");
            let disco = v.rows.iter().find_map(|r| match r {
                norte_ui_host::dto::PlaceRowView::Drive { label, hostile, .. } => {
                    Some((label.clone(), *hostile))
                }
                _ => None,
            });
            if let Some((label, hostile)) = disco {
                sin_peligro(&label, &n.id, "la etiqueta de un volumen en la barra");
                assert!(
                    hostile,
                    "[{}] la etiqueta del volumen se enmascara y NO lo dice: {label:?}",
                    n.id
                );
                break;
            }
        }

        // 3. El valor de un ATRIBUTO: el primer campo de la hoja es el nombre
        //    de la entrada bajo el cursor, o sea bytes del provider (#277).
        //    El doc de este test la nombraba desde el principio y nadie la
        //    ejercitaba.
        let foto = esperar_foto(&h, &mut sub, "la hoja tiene el nombre", |f| {
            hoja(f).is_some_and(|m| !m.fields.is_empty())
        })
        .await;
        let campo = hoja(&foto)
            .expect("la disposición `full` coloca la hoja")
            .fields
            .first()
            .expect("el primer campo es el nombre")
            .clone();
        sin_peligro(&campo.value, &n.id, "el valor de un atributo");
        assert!(
            campo.hostile,
            "[{}] el valor del atributo se enmascara y NO lo dice: {:?}",
            n.id, campo.value
        );
    }
}

/// Ninguna cadena pintable lleva un peligro de terminal.
pub(super) fn sin_peligro(pintado: &str, id: &str, donde: &str) {
    for c in pintado.chars() {
        assert!(
            !norte_encoding::is_terminal_hazard(c),
            "[{id}] {donde} lleva {c:?} sin enmascarar: {pintado:?}"
        );
    }
}
