use super::*;

// ---------------------------------------------------------------------------
// Renombrar UNA entrada (tarea 5.2).
// ---------------------------------------------------------------------------

/// `shift+F6` abre el nombre EDITABLE, sembrado con lo que la fila pinta.
#[tokio::test]
async fn renombrar_abre_el_nombre_para_editarlo() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // El cursor, sobre `notas.txt`.
    let b = listado(&snap);
    let notas = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("está");
    let (key, generation) = (notas.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");

    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    assert_eq!(d.title_key, "modal-rename-title");
    assert_eq!(
        d.input.as_deref(),
        Some("notas.txt"),
        "el campo nace con el nombre de ahora"
    );
    assert!(d.destination.is_none(), "un rename no va a otro sitio");
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "abrir el diálogo no renombra"
    );
}

/// Sin tocar el campo NO se renombra nada, y esa es la protección.
///
/// Es la regla 1 en la costura: el campo se siembra con lo que la fila PINTA,
/// y para un nombre que no es UTF-8 eso lleva un U+FFFD. Sin tocar se
/// reconstruyen los bytes ORIGINALES — que son los de ahora, o sea «mismo
/// nombre, mismo sitio»—, así que la siembra nunca puede convertirse en el
/// operando. Mandar el texto sin más escribiría mojibake de verdad.
#[tokio::test]
async fn un_nombre_sin_tocar_no_renombra_nada() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let hostil = b.rows.iter().find(|r| r.hostile).expect("hay uno");
    let (key, generation) = (hostil.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let d = siguientes_dialogos(&mut sub).await[0].clone();
    assert!(d.input_hostile, "el campo dice que lo sembrado no es fiel");

    // Se confirma SIN escribir nada. No hay renombrado posible —el destino
    // sería el mismo— y eso se DICE en el acuse, no solo en la barra.
    let ack = h
        .dispatch(UiAction::Dialog {
            id: d.id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-transfer-name-same".to_owned()
        },
        "{ack:?}"
    );
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "el mismo nombre en el mismo sitio no es una operación"
    );
}

/// Un nombre TOCADO que aún lleva el carácter de sustitución se RECHAZA:
/// confirmarlo escribiría el mojibake que la pantalla inventó.
#[tokio::test]
async fn un_nombre_tocado_con_fffd_se_rechaza() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let hostil = b.rows.iter().find(|r| r.hostile).expect("hay uno");
    let (key, generation, pintado) = (hostil.key, b.generation, hostil.display_name.clone());
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;

    // Se edita: el renderer devuelve lo que había MÁS una letra, y lo que
    // había lleva el U+FFFD que puso la pantalla.
    h.dispatch(UiAction::DialogInput {
        id,
        text: format!("{pintado}x"),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty(),
        "no se escribe un nombre que la pantalla se inventó"
    );
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.status.message.is_some_and(|m| !m.starts_with("msg-")),
        "y se dice, traducido"
    );
}

/// Un nombre nuevo sale como un `fs.move` dentro del MISMO directorio.
#[tokio::test]
async fn un_nombre_nuevo_sale_como_movimiento_al_mismo_sitio() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let notas = b
        .rows
        .iter()
        .find(|r| r.display_name == "notas.txt")
        .expect("está");
    let (key, generation) = (notas.key, b.generation);
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "apuntes.md".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let ts = anotados(&backend, "el renombrado encolado", 1, |f| {
        f.transferencias.lock().expect("transferencias").clone()
    })
    .await;
    assert_eq!(ts.len(), 1);
    let (from, to, mover, colision) = &ts[0];
    assert!(mover, "renombrar es mover");
    assert_eq!(from.to_wire(), "mem:///casa/notas.txt");
    assert_eq!(
        to.to_wire(),
        "mem:///casa/apuntes.md",
        "al MISMO directorio"
    );
    assert_eq!(*colision, norte_proto::CollisionPolicy::Fail);
}

/// Con VARIAS marcas, esta ventana se niega: cuál renombrar no lo dice
/// nadie. Es la asimetría que `Facts::rename_single` documenta, y este host
/// ya la declaraba en `hechos()`.
#[tokio::test]
async fn renombrar_con_varias_marcas_se_niega() {
    let backend = arbol();
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let b = listado(&snap);
    let generation = b.generation;
    for r in b.rows.iter().take(2) {
        h.dispatch(UiAction::ToggleMark {
            slot_id: 1,
            key: r.key,
            generation,
        })
        .await
        .expect("host vivo");
    }
    let ack = h
        .dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "reason-wrong-target".to_owned()
        },
        "{ack:?}"
    );
}

/// Y en solo lectura, ni se abre.
#[tokio::test]
async fn en_solo_lectura_renombrar_no_abre_nada() {
    let backend = arbol();
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Unavailable { .. }), "{ack:?}");
    asentar().await;
    assert!(
        backend
            .transferencias
            .lock()
            .expect("transferencias")
            .is_empty()
    );
}

// ---------------------------------------------------------------------------
// El plan de renombrado que propone un modelo (tarea 5.2).
// ---------------------------------------------------------------------------

pub(super) fn hash_de_prueba() -> norte_proto::methods::PlanHash {
    norte_proto::methods::PlanHash::parse(&"a".repeat(norte_proto::methods::PLAN_HASH_LEN))
        .expect("hex válido")
}

/// Un veredicto del core: aplicable, con `n` pasos reales.
pub(super) fn veredicto_ok(
    pares: &[(&str, &str)],
) -> norte_proto::methods::FsRenameBatchPlanResult {
    norte_proto::methods::FsRenameBatchPlanResult {
        steps: pares
            .iter()
            .map(|(f, t)| norte_proto::methods::RenameStep {
                from: norte_proto::Segment::new(f.as_bytes().to_vec()).expect("seg"),
                to: norte_proto::Segment::new(t.as_bytes().to_vec()).expect("seg"),
                temp: false,
            })
            .collect(),
        collisions: Vec::new(),
        executable: true,
        plan_hash: hash_de_prueba(),
    }
}

/// Un backend con un plan de IA y su veredicto.
pub(super) fn falso_con_plan(
    pares: &[(&str, &str)],
    veredicto: Option<norte_proto::methods::FsRenameBatchPlanResult>,
) -> Arc<Falso> {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        pares
            .iter()
            .map(|(from, _)| (from.as_bytes().to_vec(), false))
            .collect::<Vec<_>>(),
    );
    f.plan_ia = Some(
        pares
            .iter()
            .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
            .collect(),
    );
    f.veredicto = veredicto;
    Arc::new(f)
}

/// Espera la siguiente actualización que traiga la revisión del plan.
pub(super) async fn siguiente_revision(
    sub: &mut norte_ui_host::UiSubscription,
) -> Option<norte_ui_host::dto::AiRenameView> {
    for _ in 0..40 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("una actualización, no un cuelgue")
            .expect("el host sigue vivo");
        match siguiente {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::AiRename { ai_rename } = c {
                            return ai_rename.clone();
                        }
                    }
                }
                if let UiUpdate::Snapshot(s) = &m.payload
                    && s.ai_rename.is_some()
                {
                    return s.ai_rename.clone();
                }
            }
            Update::Lagged => panic!("sin retraso en este test"),
        }
    }
    panic!("la revisión no llegó");
}

/// Pide un plan: abre el prompt de la instrucción y lo contesta.
///
/// `pane.ai-rename` no lo ata ningún preset de fábrica, así que llega por la
/// paleta, que es la otra puerta del catálogo.
pub(super) async fn pedir_plan(h: &UiHost, sub: &mut norte_ui_host::UiSubscription) {
    por_la_paleta(h, sub, "ai-rename").await;
    let id = siguientes_dialogos(sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "numera los episodios".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
}

/// #310: el renombrado en lote por PLANTILLA en la ventana. El prompt fija
/// el operando (lo marcado), la plantilla se valida con el humano delante y
/// el prompt vuelve con lo tecleado y el diagnóstico en la barra, y el plan
/// —determinista, sin modelo— entra por la MISMA revisión que el de la IA,
/// con el veredicto del core en su viaje.
#[tokio::test]
async fn el_lote_por_plantilla_se_revisa_como_el_de_la_ia() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "ep02.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    // El lote actúa sobre lo MARCADO: los dos.
    por_la_paleta(&h, &mut sub, "mark.all").await;
    por_la_paleta(&h, &mut sub, "rename-batch").await;
    let prompt = foto_hasta(&h, &mut sub, "el prompt de la plantilla", |s| {
        s.dialogs
            .iter()
            .find(|d| d.title_key == "modal-rename-batch")
            .cloned()
    })
    .await;
    assert_eq!(
        prompt.input.as_deref(),
        Some("[N].[E]"),
        "prellenado con la identidad, como la TUI"
    );

    // Una plantilla que dejaría un `/` dentro se explica y el prompt VUELVE
    // con lo tecleado, en vez de tirarlo.
    h.dispatch(UiAction::DialogInput {
        id: prompt.id,
        text: "a/[N]".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: prompt.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let reabierto = foto_hasta(
        &h,
        &mut sub,
        "el prompt reabierto con el diagnóstico",
        |s| {
            s.dialogs
                .iter()
                .find(|d| d.title_key == "modal-rename-batch" && d.id != prompt.id)
                .cloned()
                .filter(|_| s.status.message.as_deref().is_some_and(|m| m.contains('/')))
        },
    )
    .await;
    assert_eq!(reabierto.input.as_deref(), Some("a/[N]"));
    assert!(
        backend.instrucciones.lock().expect("mutex").is_empty(),
        "al modelo no se le pidió nada"
    );

    // La buena: el plan se genera aquí y se revisa como el de la IA.
    h.dispatch(UiAction::DialogInput {
        id: reabierto.id,
        text: "ep0[C].[E]".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: reabierto.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let mut v = siguiente_revision(&mut sub)
        .await
        .expect("abre la revisión");
    assert_eq!(v.total, 2, "{v:?}");
    assert_eq!(v.pairs[0].from.text, "ep1.mkv");
    assert_eq!(v.pairs[0].to.text, "ep01.mkv");
    assert_eq!(v.pairs[1].to.text, "ep02.mkv");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(
        v.confirmable,
        "el core dio su veredicto sobre el plan de la plantilla"
    );
    assert!(
        backend.instrucciones.lock().expect("mutex").is_empty(),
        "sigue sin haber modelo de por medio"
    );
    assert_eq!(
        backend.veredictos_pedidos.lock().expect("mutex").len(),
        1,
        "un veredicto pedido, para el plan de la plantilla"
    );
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "revisar no aplica nada"
    );
}

/// Sobre una ubicación que rehúsa escribir, la ventana ATENÚA el borrado.
///
/// `source_read_only` y `dest_read_only` estaban cableados a `false` con un
/// comentario que declaraba que el host no lleva esa cuenta. Sí la puede
/// llevar: PIDE `capabilities` de cada hueco al aterrizar —lo hace desde
/// #268— y tiraba todo menos el modo de plegado, con el flag `READ_ONLY` a un
/// campo de distancia. El terminal sí lo mira (`App::pane_read_only`), así que
/// dentro de un zip el terminal atenuaba F5/F8 y la ventana los ofrecía
/// encendidos: la ayuda invitaba a escrituras imposibles.
///
/// Se miran las filas EJECUTABLES de la página del corpus, que es donde estos
/// hechos llegan. La hoja de teclado no vale para esto y conviene no
/// confundirlas: su `avail` es de BUILD —«este frontend implementa el
/// comando»— y no cambia con el sitio en el que esté el lector.
///
/// La prueba usa el FLAG y no el esquema a propósito. `scheme_is_read_only`
/// contesta que sí a un `zip+file://` sin preguntarle a nadie, así que un
/// test montado sobre un contenedor pasaría con el respaldo sintáctico puesto
/// y las capacidades seguidas tirándose. Un `mem:///` que anuncia `READ_ONLY`
/// —un export SFTP de solo lectura, un montaje `ro`— solo se sabe por el
/// flag.
#[tokio::test]
async fn una_ubicacion_que_rehusa_escribir_atenua_el_borrado() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    falso.capacidades.insert(
        "mem:///casa".to_owned(),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        },
    );
    let (h, _snap) = host_en(Arc::new(falso), "mem:///casa").await;
    let mut sub = h.subscribe();
    // Las capacidades se piden al aterrizar el listado y vuelven por su
    // cuenta: la ayuda congela los hechos AL ABRIRSE, así que abrirla antes
    // de que lleguen congelaría el «no consta» de siempre.
    asentar().await;

    // F8 borra en el hueco ACTIVO, que es el origen: es la tecla que
    // pregunta por `source_read_only` y solo por él.
    let pagina = pagina_de_copiado(&h, &mut sub).await;
    let fila = accion(&pagina, "F8");
    assert!(
        !fila.enabled,
        "aquí no se puede borrar y la página lo ofrece apagado: {fila:?}"
    );
    assert_eq!(
        fila.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "reason-read-only"),
        "y dice POR QUÉ, no solo que no: {fila:?}"
    );
}

/// Y un re-listado por debajo NO devuelve la ayuda al «no consta».
///
/// Las capacidades se BORRABAN al pedirlas, y el aterrizaje re-congela los
/// hechos de la ayuda tres líneas después: o sea que toda re-congelación que
/// saliera de un listado leía `None` —siempre, no a veces— y la fila volvía a
/// encenderse. Con la ayuda delante basta con que termine una tarea o que
/// salte el watcher para que F8 pase de atenuada a encendida sin que el sitio
/// haya cambiado.
///
/// Ahora la respuesta va ATADA a su ruta y no se tira al pedir otra: solo un
/// cambio de directorio la invalida, que es lo único que de verdad la
/// invalida.
#[tokio::test]
async fn un_relistado_no_reenciende_lo_que_el_sitio_sigue_rehusando() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    falso.capacidades.insert(
        "mem:///casa".to_owned(),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        },
    );
    let (h, _snap) = host_en(Arc::new(falso), "mem:///casa").await;
    let mut sub = h.subscribe();
    asentar().await;
    let antes = pagina_de_copiado(&h, &mut sub).await;
    assert!(
        !accion(&antes, "F8").enabled,
        "la premisa: con las capacidades puestas, apagada"
    );

    // Un re-listado del hueco, que es lo que hace por debajo el fin de una
    // tarea o el watcher mientras la ayuda sigue abierta.
    h.dispatch(UiAction::RefreshSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    asentar().await;

    let despues = foto_hasta(&h, &mut sub, "la ayuda tras el re-listado", |s| {
        s.help.clone()
    })
    .await;
    assert!(
        !accion(&despues, "F8").enabled,
        "el sitio no ha cambiado: re-listar no puede encender lo que rehúsa \
         escribir ({:?})",
        accion(&despues, "F8")
    );
}

/// Y el DESTINO se pregunta al hueco del destino, no al que tiene el foco.
///
/// La otra mitad del hecho, y la que un solo hueco no puede probar: con el
/// origen escribible y el destino de solo lectura, F5 —que escribe allí— se
/// apaga y F8 —que escribe aquí— sigue encendida. Un `source_read_only`
/// copiado al `dest_read_only` pasaría el test de arriba y fallaría éste.
#[tokio::test]
async fn el_destino_de_solo_lectura_atenua_la_copia_y_no_el_borrado() {
    let mut falso = Falso::default();
    falso.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"notas.txt".to_vec(), false)],
    );
    falso.pon("mem:///casa/docs", vec![(b"x.md".to_vec(), false)]);
    falso.capacidades.insert(
        "mem:///casa/docs".to_owned(),
        norte_proto::Capabilities {
            flags: norte_proto::CapabilityFlags::READ_ONLY,
            max_path: None,
        },
    );
    let (h, _snap) = dos_paneles_con_destino_aparte(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    // El helper deja el foco en el hueco que navegó. El caso es el otro: el
    // lector está en `/casa`, que escribe, mirando a `/casa/docs`, que no.
    h.dispatch(UiAction::FocusSlot { slot_id: 1 })
        .await
        .expect("host vivo");
    asentar().await;

    let pagina = pagina_de_copiado(&h, &mut sub).await;
    let copiar = accion(&pagina, "F5");
    assert!(!copiar.enabled, "el destino no acepta la copia: {copiar:?}");
    assert_eq!(
        copiar.reason,
        norte_i18n::t_in(norte_i18n::Lang::Es, "reason-read-only"),
        "{copiar:?}"
    );

    let borrar = accion(&pagina, "F8");
    assert!(
        borrar.enabled,
        "borrar es en el ORIGEN, que sí escribe: atenuarlo sería contar el \
         impedimento del hueco equivocado ({borrar:?})"
    );
}

/// Con el cursor sobre un `.zip`, la ayuda ofrece `Enter`: es lo que hace.
///
/// El otro hecho que decía otra cosa que la tecla. `enterable` preguntaba
/// `kind == Dir`, así que la página de archivos —cuya primera frase es
/// literalmente «Enter sobre un archivo comprimido entra en él»— ofrecía esa
/// misma fila apagada y con «no aplica a esto». Ahora lo contesta el sitio
/// compartido, que es el mismo que navega (ADR 0077).
#[tokio::test]
async fn con_el_cursor_en_un_zip_la_ayuda_ofrece_entrar() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"cosas.zip".to_vec(), false)]);
    let (h, snap) = host_en(Arc::new(falso), "mem:///casa").await;
    let mut sub = h.subscribe();
    let b = listado_de(&snap, 1);
    let zip = b
        .rows
        .iter()
        .find(|r| r.display_name == "cosas.zip")
        .expect("el archivo está");
    h.dispatch(UiAction::SelectRow {
        slot_id: 1,
        key: zip.key,
        generation: b.generation,
    })
    .await
    .expect("host vivo");

    let pagina = pagina_de_ayuda(&h, &mut sub, "archives").await;
    let entrar = accion(&pagina, "Enter");
    assert!(
        entrar.enabled,
        "la página dice que Enter entra en un comprimido, y la fila lo \
         ofrecía apagada: {entrar:?}"
    );
}

/// Abre la ayuda en la página de copiar, borrar y renombrar.
///
/// Es la página del corpus que documenta las cuatro teclas que estos hechos
/// atenúan, y sus filas EJECUTABLES son donde los hechos congelados llegan
/// —lo que el lector ve—. La hoja de teclado no sirve: su `avail` es de
/// build, no del sitio en el que está el lector.
pub(super) async fn pagina_de_copiado(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
) -> norte_ui_host::dto::HelpView {
    pagina_de_ayuda(h, sub, "copying").await
}

/// Abre la ayuda y recorre la lateral hasta una página, como el lector.
pub(super) async fn pagina_de_ayuda(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    topico: &str,
) -> norte_ui_host::dto::HelpView {
    let mut pagina = abrir_ayuda(h, sub).await;
    let mut row = 0;
    // Sobre la longitud VIGENTE: la lateral crece cuando aterriza el catálogo
    // de extensiones, así que la de la primera foto se queda corta.
    while row < u32::try_from(pagina.sidebar.len()).expect("cabe") {
        if pagina.topic_id == topico {
            return pagina;
        }
        h.dispatch(UiAction::HelpSelectTopic { row })
            .await
            .expect("host vivo");
        pagina = siguiente_ayuda(sub).await.expect("sigue abierta");
        row += 1;
    }
    assert_eq!(pagina.topic_id, topico, "la lateral trae la página");
    pagina
}

/// La fila ejecutable de un acorde en una página ya abierta.
pub(super) fn accion<'p>(
    pagina: &'p norte_ui_host::dto::HelpView,
    chord: &str,
) -> &'p norte_ui_host::dto::HelpActionView {
    pagina
        .actions
        .iter()
        .find(|a| !a.opens_topic && a.chord == chord)
        .unwrap_or_else(|| {
            panic!(
                "la página `{}` documenta {chord}: {:?}",
                pagina.topic_id, pagina.actions
            )
        })
}

/// `Enter` sobre un ARCHIVO comprimido entra en él, no lo abre fuera.
///
/// La divergencia número uno del inventario de paridad: el terminal navega al
/// `zip+file://…/!/` y la ventana se lo daba a `xdg-open`. El host miraba
/// `kind != Dir` y ahí se acababa la pregunta — mientras su propio comentario
/// afirmaba que hacía «la misma decisión que el TUI» (ADR 0077), que es
/// exactamente la afirmación falsa que esa ADR existe para impedir.
///
/// La ventana YA conocía `archive_root_for`: la usa para desempaquetar y para
/// comprobar un contenedor. Lo que faltaba era preguntársela al abrir.
#[tokio::test]
async fn entrar_en_un_archivo_comprimido_navega_dentro() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"cosas.zip".to_vec(), false)]);
    let (h, snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let primero = listado(&snap);

    h.dispatch(UiAction::Activate {
        slot_id: primero.slot_id,
        key: norte_ui_host::RowKey(0),
        generation: primero.generation,
    })
    .await
    .expect("host vivo");

    let dentro = foto_hasta(&h, &mut sub, "el listado dentro del archivo", |s| {
        let b = listado(s);
        b.path_display
            .contains("zip")
            .then(|| b.path_display.clone())
    })
    .await;
    assert!(
        dentro.contains("cosas.zip"),
        "se entra en el contenedor, no se entrega al escritorio: {dentro}"
    );
}

/// Y sobre un SYMLINK se navega, como en el terminal.
///
/// La otra mitad de la misma divergencia: el host lo trataba como «no es un
/// directorio», o sea como un fichero, así que un enlace a una carpeta se
/// entregaba al escritorio en vez de entrar en ella.
#[tokio::test]
async fn entrar_en_un_symlink_navega_como_en_el_terminal() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"atajo".to_vec(), false)]);
    falso.pon_kind("mem:///casa/atajo", norte_proto::EntryKind::Symlink);
    falso.pon("mem:///casa/atajo", vec![(b"dentro.txt".to_vec(), false)]);
    let (h, snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let primero = listado(&snap);

    let ack = h
        .dispatch(UiAction::Activate {
            slot_id: primero.slot_id,
            key: norte_ui_host::RowKey(0),
            generation: primero.generation,
        })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "ack fue {ack:?}");

    let dentro = foto_hasta(&h, &mut sub, "el listado del enlace", |s| {
        let b = listado(s);
        b.path_display
            .ends_with("atajo")
            .then(|| b.path_display.clone())
    })
    .await;
    assert!(dentro.ends_with("atajo"), "{dentro}");
}

/// `[ui] confirm_quit` también pregunta en la VENTANA.
///
/// Cerrarla no preguntaba NUNCA: el manejador de `CloseRequested` volcaba la
/// sesión y cerraba. Con `confirm_quit = "always"` el terminal guarda F10 y la
/// ventana se iba con una copia a medias sin decir nada — y `always` es
/// justo el valor que pide la guarda.
///
/// La decisión de si hay que preguntar es la COMPARTIDA
/// (`settings::quit_needs_confirm`), cuyo rustdoc ya nombraba a un
/// `confirm_quit_should_open` de la ventana que no existía.
#[tokio::test]
async fn cerrar_la_ventana_pregunta_si_la_config_lo_dice() {
    let mut cfg = ajustes_de_prueba();
    cfg.common.ui_confirm_quit = norte_config::ConfirmQuit::Always;
    let (h, _snap) = host_en_con(arbol(), "mem:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    let ack = h.dispatch(UiAction::RequestQuit).await.expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let dialogo = foto_hasta(&h, &mut sub, "el diálogo de salir", |s| {
        s.dialogs.first().cloned()
    })
    .await;
    assert_eq!(dialogo.title_key, "modal-quit-title");
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), efectos.recv())
            .await
            .is_err(),
        "preguntar NO cierra: el efecto de cerrar sale al confirmar"
    );

    // Y al confirmar, ahí sí.
    h.dispatch(UiAction::Dialog {
        id: dialogo.id,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    assert!(
        matches!(efecto, norte_ui_host::dto::NativeEffect::CloseWindow),
        "{efecto:?}"
    );
}

/// Con `confirm_quit = "never"` no se pregunta: se cierra y ya.
#[tokio::test]
async fn sin_confirmacion_cerrar_no_abre_nada() {
    let mut cfg = ajustes_de_prueba();
    cfg.common.ui_confirm_quit = norte_config::ConfirmQuit::Never;
    let (h, _snap) = host_en_con(arbol(), "mem:///casa", cfg).await;
    let mut efectos = h.native_effects();

    h.dispatch(UiAction::RequestQuit).await.expect("host vivo");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    assert!(
        matches!(efecto, norte_ui_host::dto::NativeEffect::CloseWindow),
        "{efecto:?}"
    );
}

/// `F10` —`app.quit` en los siete presets— cierra la ventana por el MISMO
/// camino que el botón de cerrar. Estaba clasificado como «no aplica a una
/// ventana», y el lector pulsaba la tecla de salir de siempre sin que pasara
/// nada.
#[tokio::test]
async fn la_tecla_de_salir_cierra_la_ventana() {
    let mut cfg = ajustes_de_prueba();
    cfg.common.ui_confirm_quit = norte_config::ConfirmQuit::Never;
    let (h, _snap) = host_en_con(arbol(), "mem:///casa", cfg).await;
    let mut efectos = h.native_effects();

    let ack = h.dispatch(tecla("F10")).await.expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    assert!(
        matches!(efecto, norte_ui_host::dto::NativeEffect::CloseWindow),
        "{efecto:?}"
    );
}

/// `[ui] quick_search` elige el modo también en la VENTANA.
///
/// El host arrancaba el buscador incremental en `Filter` a fuego, así que
/// `quick_search = "jump"` movía el cursor en `ntc` y acotaba el listado en la
/// ventana: la misma clave con dos comportamientos. El DTO ya sabía decir los
/// dos modos; lo que faltaba era leer la clave.
#[tokio::test]
async fn el_modo_del_buscador_rapido_sale_de_la_config() {
    let mut cfg = ajustes_de_prueba();
    cfg.quick_search_mode = norte_frontend::nav::Mode::Jump;
    let (h, _snap) = host_en_con(arbol(), "mem:///casa", cfg).await;
    let mut sub = h.subscribe();

    ejecutar_por_paleta(&h, &mut sub, "pane.quick-search").await;
    let modo = foto_hasta(&h, &mut sub, "el buscador abierto", |s| {
        listado(s).quick.as_ref().map(|q| q.mode.clone())
    })
    .await;
    assert_eq!(modo, "jump", "el modo lo dice la configuración");
}

/// `[ui.columns]` estiliza las columnas también en la VENTANA (#108).
///
/// La ventana pedía el estilo con `ColumnStyle::default_for_id`, o sea el de
/// FÁBRICA, en la cabecera y en las celdas. Así que la lista de columnas y su
/// orden salían de la configuración y todo lo demás —`header` propia,
/// `format`, `align`, `width`— estaba muerto: un bloque de configuración
/// entero vivo en el terminal y sin efecto aquí.
///
/// Se comprueban las dos puertas a la vez, que son las dos que estaban mal:
/// la cabecera con un rótulo propio y la celda con `format = "iso"`.
#[tokio::test]
async fn el_estilo_por_columna_manda_en_la_ventana() {
    let mut falso = Falso::default();
    falso.pon("mem:///casa", vec![(b"a.txt".to_vec(), false)]);
    let cfg = norte_config::ColumnsConfig {
        default_columns: Some(vec!["name".to_owned(), "mtime".to_owned()]),
        specs: [(
            "mtime".to_owned(),
            norte_config::ColumnSpec {
                width: None,
                align: None,
                format: Some("iso".to_owned()),
                header: Some("Cuándo".to_owned()),
            },
        )]
        .into_iter()
        .collect(),
        ..norte_config::ColumnsConfig::default()
    };
    let columnas = norte_frontend::columns::ColumnsSettings::resolve(&cfg);
    let (h, snap) = UiHost::start(UiHostOptions {
        backend: Arc::new(falso),
        initial_dir: dir(),
        initial_dir_pedido: false,
        locale: "es".to_owned(),
        keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
        keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
        keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox").expect("preset"),
        layout: norte_frontend::layout::presets::tree("simple").expect("layout"),
        viewport: (120, 40),
        settings: ajustes_de_prueba(),
        paths: norte_ui_host::settings::HostPaths::default(),
        theme: norte_ui_host::pickers::HostTheme::default(),
        user_layouts: Vec::new(),
        profile: None,
        columns: columnas,
        effects: norte_ui_host::commands::Efectos::Completo,
        log_ring: None,
    })
    .await
    .expect("arranca");
    let mut sub = h.subscribe();

    let cabecera = listado(&snap)
        .columns
        .iter()
        .find(|c| c.id == "mtime")
        .expect("la columna está")
        .clone();
    assert_eq!(
        cabecera.label, "Cuándo",
        "el rótulo propio manda sobre el de fábrica"
    );

    // La otra mitad del arreglo —el `format` de una CELDA— se comprueba en
    // `norte-gui-tauri/tests/celdas_locales.rs`: aquí el backend falso no
    // trae fecha en el listado (#52, el listado es perezoso) y hidratarla
    // pedía montar medio sondeo para probar un formato. Allí hay ficheros de
    // verdad, que es donde esa pregunta se contesta sola.
    let _ = (&h, &mut sub);
}

/// `openers.toml` manda también en la VENTANA (#28).
///
/// La tabla de openers la leía solo el terminal: la ventana entregaba todo al
/// manejador del escritorio, así que una regla que dice «los PDF con zathura»
/// valía en `ntc` y no valía en `norte-gui`. Es una feature documentada
/// entera honrada por una sola superficie.
#[tokio::test]
async fn un_opener_declarado_manda_en_la_ventana() {
    let mut falso = Falso::default();
    falso.pon("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let mut cfg = ajustes_de_prueba();
    cfg.openers = norte_frontend::openers::OpenersConfig::parse(
        "[[opener]]\nmime = \"text/*\"\ncommand = [\"cat\", \"%f\"]\ndetached = false\n",
    )
    .expect("config de test");
    let (h, _snap) = host_en_con(Arc::new(falso), "file:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "pane.open").await;
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    let norte_ui_host::dto::NativeEffect::RunProgram { argv, cwd, .. } = efecto else {
        panic!("con una regla declarada se corre ESE programa, no el del escritorio: {efecto:?}");
    };
    let como_texto: Vec<String> = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(
        como_texto[0].ends_with("/cat"),
        "el programa va resuelto a ruta absoluta ANTES de darle cwd (ADR 0082): {como_texto:?}"
    );
    assert!(
        como_texto[1].ends_with("/casa/notas.txt"),
        "y `%f` es el fichero señalado: {como_texto:?}"
    );
    assert_eq!(
        cwd.map(|c| String::from_utf8_lossy(&c).into_owned()),
        Some("/casa".to_owned()),
        "el hijo abre en el directorio que se está mirando (#144)"
    );
}

/// Sin regla para ese mimetype queda el manejador del ESCRITORIO.
///
/// El último recurso es el de siempre: escribir configuración no puede ser
/// requisito para abrir un PDF.
#[tokio::test]
async fn sin_regla_declarada_abre_con_el_escritorio() {
    let mut falso = Falso::default();
    falso.pon("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let (h, _snap) = host_en(Arc::new(falso), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "pane.open").await;
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    assert!(
        matches!(efecto, norte_ui_host::dto::NativeEffect::OpenPath { .. }),
        "sin regla, el escritorio: {efecto:?}"
    );
}

/// `[ui] editor` manda en F4, y si no hay, el manejador del escritorio.
///
/// La ventana mandaba `pane.edit` al mismo sitio que `pane.open` SIEMPRE. La
/// parte deliberada de esa decisión es no lanzar `$EDITOR` —un editor de
/// terminal dentro de una ventana que no tiene terminal—, y sigue en pie.
/// Lo que no era deliberado es ignorar `[ui] editor`, que nombra un programa
/// explícito y puede perfectamente ser gráfico: su clave hermana `[ui] diff`
/// SÍ la honra esta ventana.
#[tokio::test]
async fn el_editor_configurado_manda_en_la_ventana() {
    let mut falso = Falso::default();
    falso.pon("file:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let mut cfg = ajustes_de_prueba();
    cfg.common.ui_editor = Some(vec!["cat".to_owned(), "%f".to_owned()]);
    let (h, _snap) = host_en_con(Arc::new(falso), "file:///casa", cfg).await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    ejecutar_por_paleta(&h, &mut sub, "pane.edit").await;
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    let norte_ui_host::dto::NativeEffect::RunProgram { argv, .. } = efecto else {
        panic!("con `[ui] editor` puesto se corre ESE editor: {efecto:?}");
    };
    let como_texto: Vec<String> = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(como_texto[0].ends_with("/cat"), "{como_texto:?}");
    assert!(como_texto[1].ends_with("/casa/notas.txt"), "{como_texto:?}");
}

/// #312: comparar dos ficheros desde la ventana. El operando y el programa
/// son las decisiones compartidas con la TUI (`diffpair`, `[ui] diff`,
/// `diff -u` por defecto); lo que cambia es que quien hospeda corre el
/// programa ESPERÁNDOLO —el argv sale resuelto e interpolado, en bytes— y lo
/// que imprimió vuelve como acción y se enseña en su panel hasta que se
/// cierra.
#[tokio::test]
async fn comparar_dos_ficheros_corre_el_comparador_y_ensena_su_salida() {
    let mut falso = Falso::default();
    falso.pon(
        "file:///casa",
        vec![(b"a.txt".to_vec(), false), (b"b.txt".to_vec(), false)],
    );
    let backend = Arc::new(falso);
    let (h, _snap) = host_en(Arc::clone(&backend), "file:///casa").await;
    let mut sub = h.subscribe();
    let mut efectos = h.native_effects();

    // Con UNO solo bajo el cursor y nada enfrente, el comando lo DICE.
    // `alt+C` es su atajo en el preset ortodoxo.
    let ack = h
        .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
            key: "C".to_owned(),
            ctrl: false,
            alt: true,
            shift: false,
            meta: false,
        }))
        .await
        .expect("host vivo");
    assert!(
        matches!(ack, ActionAck::Unavailable { ref reason_key } if reason_key == "msg-compare-files-need-two"),
        "fue {ack:?}"
    );

    // Marcados los dos: sale el efecto con el argv por defecto, resuelto.
    por_la_paleta(&h, &mut sub, "mark.all").await;
    ejecutar_por_paleta(&h, &mut sub, "pane.compare-files").await;
    let efecto = tokio::time::timeout(std::time::Duration::from_secs(2), efectos.recv())
        .await
        .expect("sale el efecto")
        .expect("canal vivo");
    let norte_ui_host::dto::NativeEffect::RunProgram {
        title_key,
        argv,
        cwd,
        detached,
    } = efecto
    else {
        panic!("se esperaba correr un programa: {efecto:?}");
    };
    assert_eq!(title_key, "program-output-compare");
    assert!(
        !detached,
        "`diff -u` se espera: su salida es lo que se enseña"
    );
    let como_texto: Vec<String> = argv
        .iter()
        .map(|a| String::from_utf8_lossy(a).into_owned())
        .collect();
    assert!(
        como_texto[0].ends_with("/diff"),
        "el programa va resuelto a ruta absoluta (ADR 0082): {como_texto:?}"
    );
    assert_eq!(
        &como_texto[1..],
        ["-u", "/casa/a.txt", "/casa/b.txt"],
        "las DOS rutas nativas, interpoladas por `%F`"
    );
    assert_eq!(cwd.as_deref(), Some(b"/casa".as_slice()));

    // Lo que imprimió vuelve como acción y se enseña, por líneas y
    // enmascarado; Esc lo cierra.
    h.dispatch(UiAction::ProgramFinished {
        title_key,
        command: como_texto.join(" "),
        output: b"--- a.txt\n+++ b.txt\n-hola\x1b[31m\n+adios\n".to_vec(),
        truncated: false,
        failed: false,
    })
    .await
    .expect("host vivo");
    let con_salida = foto_hasta(&h, &mut sub, "la salida del programa", |s| {
        s.program_output.clone()
    })
    .await;
    assert_eq!(con_salida.title_key, "program-output-compare");
    assert_eq!(con_salida.lines.len(), 4, "{con_salida:?}");
    assert_eq!(con_salida.lines[0], "--- a.txt");
    assert!(
        con_salida.text_hostile,
        "el escape de la tercera línea se marcó"
    );
    assert!(!con_salida.lines[2].contains('\x1b'));
    assert!(!con_salida.failed);
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    foto_hasta(&h, &mut sub, "el panel cerrado", |s| {
        s.program_output.is_none().then_some(())
    })
    .await;
}

/// El plan se REVISA antes de nada: llega, se pinta pareja a pareja, y el
/// veredicto del core llega DESPUÉS, en su propio viaje.
#[tokio::test]
async fn un_plan_se_revisa_antes_de_aplicarse() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "ep02.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    let primera = siguiente_revision(&mut sub).await.expect("abre");
    assert_eq!(primera.total, 2, "las dos parejas");
    assert_eq!(primera.pairs[0].from.text, "ep1.mkv");
    assert_eq!(primera.pairs[0].to.text, "ep01.mkv");
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "revisar no aplica nada"
    );

    // El veredicto llega en su propio viaje, y hasta entonces no se puede
    // aprobar.
    let mut v = primera;
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(v.confirmable, "el core dijo que es aplicable");
    assert!(
        v.real_steps_note.contains('2'),
        "y cuántos renombra DE VERDAD, dicho y traducido: {}",
        v.real_steps_note
    );
    assert!(!v.status.is_empty() && !v.status.starts_with("modal-"));
}

/// Aprobar manda UNA task para el lote, con el `plan_hash` que devolvió el
/// core: se ejecuta EXACTAMENTE lo que se enseñó.
#[tokio::test]
async fn aprobar_manda_el_lote_con_el_hash_del_core() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }

    // La PRIMERA tecla solo reconoce la pantalla: se abrió sola y se quedó
    // el teclado, así que la tecla que venía en camino no puede ser una
    // respuesta. La segunda ya aprueba.
    h.dispatch(tecla("y")).await.expect("host vivo");
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "la primera tecla no aprueba nada"
    );
    h.dispatch(tecla("y")).await.expect("host vivo");
    let lotes = anotados(&backend, "el lote aprobado", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(lotes.len(), 1, "UNA task para el lote entero");
    let (dir, parejas, hash) = &lotes[0];
    assert_eq!(dir.to_wire(), "mem:///casa");
    assert_eq!(parejas.len(), 1);
    assert_eq!(parejas[0].from.as_bytes(), b"ep1.mkv");
    assert_eq!(parejas[0].to.as_bytes(), b"ep01.mkv");
    assert_eq!(hash, &hash_de_prueba(), "el hash es el que dio el core");
}

/// Un plan que el core NO acepta no se puede aprobar, y se dice.
#[tokio::test]
async fn un_plan_no_aplicable_no_se_aprueba() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut v = veredicto_ok(&pares);
    v.executable = false;
    v.steps.clear();
    v.collisions = vec![norte_proto::methods::RenameCollision {
        pair_index: 0,
        kind: norte_proto::methods::RenameCollisionKind::External,
        name: norte_proto::Segment::new(b"ep01.mkv".to_vec()).expect("seg"),
    }];
    let backend = falso_con_plan(&pares, Some(v));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut r = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if !r.detail.is_empty() {
            break;
        }
        r = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(!r.confirmable, "el core dijo que no");
    assert!(!r.detail.is_empty(), "y la colisión se LEE: {:?}", r.detail);

    // La primera tecla reconoce la pantalla; la segunda intenta aprobar.
    h.dispatch(tecla("y")).await.expect("host vivo");
    let ack = h.dispatch(tecla("y")).await.expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-plan-not-applicable".to_owned()
        },
        "{ack:?}"
    );
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Una pareja que no es un nombre legal tumba el plan ENTERO: aplicar «lo que
/// valga» de un plan adulterado es lo que este cinturón existe para impedir.
#[tokio::test]
async fn una_pareja_invalida_tumba_el_plan_entero() {
    let pares = [("ep1.mkv", "ep01.mkv"), ("ep2.mkv", "../fuera")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&[("ep1.mkv", "ep01.mkv")])));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    // El plan del modelo ya volvió: lo que se comprueba es lo que el host
    // hace CON él, no que todavía no haya llegado.
    hasta(&backend, "el plan del modelo, ya servido", |f| {
        f.en_calma().then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.ai_rename.is_none(),
        "ni se abre la revisión: {:?}",
        foto.ai_rename
    );
    assert!(
        backend
            .veredictos_pedidos
            .lock()
            .expect("veredictos")
            .is_empty(),
        "ni se le pide veredicto al core a un plan adulterado"
    );
    assert!(foto.status.message.is_some(), "y se dice");
}

/// Un plan que llega TARDE, después de que el lector cerrara la revisión, no
/// la reabre.
///
/// El modelo tarda, y en esa ventana el lector puede descartar. Sin subir la
/// época al cerrar, el plan aterrizaba encima de una pantalla que su dueño ya
/// había quitado — y con las teclas puestas sobre él.
#[tokio::test]
async fn un_plan_que_llega_tarde_no_reabre_lo_cerrado() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 150;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    // Antes de que el modelo conteste, se descarta. Y se espera a que el
    // plan tardío HAYA llegado: sin eso, el test pasaría por no haber
    // esperado bastante, que es la forma más silenciosa de no probar nada.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    hasta(&backend, "el plan tardío, ya servido", |f| {
        f.en_calma().then_some(())
    })
    .await;
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        foto.ai_rename.is_none(),
        "el plan tardío no reabre lo cerrado: {:?}",
        foto.ai_rename
    );
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Descartar no aplica nada, y la revisión se queda cerrada.
#[tokio::test]
async fn descartar_cierra_y_no_aplica_nada() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    asentar().await;
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.ai_rename.is_none(), "se cerró y sigue cerrada");
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Los nombres del plan los propone un MODELO sobre nombres que escribió
/// cualquiera: se enmascaran y se DICE.
#[tokio::test]
async fn un_nombre_hostil_del_plan_va_marcado() {
    // Del corpus canónico, no escrito a mano: un nombre inventado en el test
    // prueba lo que el test cree, y el corpus prueba lo que de verdad hay.
    let bytes = hostil("rtl_override");
    let alterado = String::from_utf8(bytes).expect("el del corpus es UTF-8");
    let pares = [("ep1.mkv", alterado.as_str())];
    let backend = falso_con_plan(&pares, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert!(
        !r.pairs[0].to.text.contains('\u{202E}'),
        "enmascarado: {:?}",
        r.pairs[0].to
    );
    assert!(r.pairs[0].to.hostile, "y marcado: {:?}", r.pairs[0].to);
    assert!(!r.pairs[0].from.hostile, "el de origen no lo es");
}

/// Un plan más largo que la ventana se recorre entero.
#[tokio::test]
async fn un_plan_largo_se_recorre() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert_eq!(r.total, 12);
    assert!(
        r.pairs.len() < 12,
        "solo la ventana viaja: {}",
        r.pairs.len()
    );
    assert_eq!(r.first_visible, 0);

    // La primera tecla reconoce la pantalla; a partir de ahí se recorre.
    h.dispatch(tecla("PageDown")).await.expect("host vivo");
    h.dispatch(tecla("PageDown")).await.expect("host vivo");
    // El veredicto del core viaja por el MISMO canal ordenado, así que puede
    // haber una actualización suya por delante de la del recorrido.
    let mut bajado = siguiente_revision(&mut sub).await.expect("sigue abierta");
    for _ in 0..10 {
        if bajado.first_visible > 0 {
            break;
        }
        bajado = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(bajado.first_visible > 0, "se recorrió: {bajado:?}");

    for _ in 0..10 {
        h.dispatch(tecla("PageDown")).await.expect("host vivo");
    }
    let tope = siguiente_revision(&mut sub).await.expect("sigue abierta");
    assert!(
        tope.first_visible + tope.pairs.len() as u64 <= tope.total,
        "la ventana no se sale del plan: {tope:?}"
    );
}

/// En solo lectura no se le pide un plan a nadie.
#[tokio::test]
async fn en_solo_lectura_no_se_pide_plan() {
    let backend = falso_con_plan(&[("a", "b")], None);
    let (h, _snap) = host_solo_lectura(Arc::clone(&backend)).await;
    // Ni la paleta lo ofrece: una ventana de solo lectura no lista lo que
    // muta. Y aunque llegara por otra puerta, la guarda lo rehúsa.
    let mut sub = h.subscribe();
    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let p = siguiente_paleta(&mut sub).await.expect("la paleta abre");
    assert!(
        !p.rows.iter().any(|r| r.text == "pane.ai-rename"),
        "una ventana de solo lectura no ofrece pedir un plan"
    );
    asentar().await;
    assert!(
        backend
            .instrucciones
            .lock()
            .expect("instrucciones")
            .is_empty()
    );
}

/// Un plan se abre sobre el directorio para el que se PIDIÓ, aunque el lector
/// haya navegado mientras el modelo pensaba.
///
/// Leer el directorio del hueco al aterrizar prometía renombrar lo que se ve
/// —que ya es otra cosa— y habría renombrado lo de antes.
#[tokio::test]
async fn un_plan_se_abre_sobre_el_directorio_que_se_planeo() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![(b"docs".to_vec(), true), (b"ep1.mkv".to_vec(), false)],
    );
    f.pon("mem:///casa/docs", vec![(b"a.md".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.retraso_ia_ms = 150;
    let backend = Arc::new(f);
    let (h, snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let b = listado(&snap);
    let docs = b
        .rows
        .iter()
        .find(|r| r.display_name == "docs")
        .expect("está");
    let (key, generation) = (docs.key, b.generation);
    pedir_plan(&h, &mut sub).await;

    // Y mientras el modelo piensa, el lector se va a otro directorio.
    h.dispatch(UiAction::Activate {
        slot_id: 1,
        key,
        generation,
    })
    .await
    .expect("host vivo");

    let r = siguiente_revision(&mut sub).await.expect("abre igual");
    assert!(
        r.dir.text.ends_with("/casa"),
        "el plan es del directorio que se planeó, no del que se ve ahora: {:?}",
        r.dir
    );
}

/// DOS peticiones vivas a la vez: la respuesta de la primera no puede matar a
/// la segunda.
///
/// `Option::take` vacía el hueco ANTES de que el filtro mire, así que una
/// respuesta vieja se llevaba por delante la petición viva y se quedaban las
/// DOS sin abrir — sin decir nada, y sin poder distinguirse de un daemon
/// muerto. Y la secuencia es la normal: pedir, no ver nada, volver a pedir.
#[tokio::test]
async fn dos_peticiones_a_la_vez_y_la_segunda_sigue_abriendo() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Dos peticiones seguidas, sin esperar a la primera.
    pedir_plan(&h, &mut sub).await;
    pedir_plan(&h, &mut sub).await;

    let r = siguiente_revision(&mut sub).await.expect("la segunda abre");
    assert_eq!(r.total, 1);
    assert_eq!(
        backend.instrucciones.lock().expect("instrucciones").len(),
        2,
        "se pidieron las dos"
    );
}

/// Con un plan en vuelo, `Escape` cierra la PALETA y no mata el plan.
///
/// La rama que abandona el plan estaba por encima del reparto de overlays, así
/// que una sola tecla hacía dos cosas mal: dejaba la paleta abierta y se
/// llevaba por delante el plan que el lector sí quería.
#[tokio::test]
async fn con_un_plan_en_vuelo_escape_cierra_la_paleta() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    h.dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
        key: "p".to_owned(),
        ctrl: true,
        alt: false,
        shift: false,
        meta: false,
    }))
    .await
    .expect("host vivo");
    let _ = siguiente_paleta(&mut sub).await.expect("abre");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.palette.is_none(), "el Escape cerró la paleta");

    // Y el plan sigue vivo: llega y abre.
    let r = siguiente_revision(&mut sub)
        .await
        .expect("el plan sobrevivió");
    assert_eq!(r.total, 1);
}

/// Lo mismo con el filtro rápido: `Escape` lo cancela, y el plan sigue.
#[tokio::test]
async fn con_un_plan_en_vuelo_escape_cancela_el_filtro() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;

    // `pane.quick-search` es `ctrl+s` en el preset ortodoxo; se llega por la
    // paleta para no depender de la tecla.
    por_la_paleta(&h, &mut sub, "quick-search").await;
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(
        listado(&foto).quick.is_none(),
        "el Escape canceló el filtro"
    );
    // Por la FOTO y no esperando el parche de la revisión: el
    // `siguiente_foto` de arriba consume sobres hasta encontrar una foto, y
    // con `retraso_ia_ms = 120` el parche de la revisión puede caer justo
    // ahí — se lo tragaba y luego esperaba para siempre un evento que ya
    // había pasado (rojo intermitente bajo carga). Un `Resync` reenvía el
    // ESTADO, así que preguntar por la foto no puede perderse nada.
    let r = foto_hasta(&h, &mut sub, "la revisión del plan aterrizó", |foto| {
        foto.ai_rename.clone()
    })
    .await;
    assert_eq!(r.total, 1);
}

/// Descartar una revisión no mata una petición POSTERIOR.
///
/// Con una revisión abierta, el teclado es suyo — así que la única forma de
/// tener dos peticiones y una revisión a la vez es la real: se pide la
/// primera, se abre el prompt de la segunda mientras el modelo piensa, la
/// primera revisión aterriza DEBAJO de ese diálogo, se confirma la segunda
/// petición, y solo entonces se descarta la revisión que quedó a la vista.
/// Soltar ahí la petición en vuelo la mataba en silencio.
#[tokio::test]
async fn descartar_una_revision_no_mata_la_peticion_siguiente() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"ep1.mkv".to_vec(), false)]);
    f.plan_ia = Some(vec![("ep1.mkv".to_owned(), "ep01.mkv".to_owned())]);
    f.veredicto = Some(veredicto_ok(&pares));
    f.retraso_ia_ms = 120;
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    // Petición 1, y el prompt de la 2 abierto mientras el modelo piensa.
    pedir_plan(&h, &mut sub).await;
    por_la_paleta(&h, &mut sub, "ai-rename").await;
    let id2 = siguientes_dialogos(&mut sub).await[0].id;

    // La revisión 1 aterriza DEBAJO del diálogo.
    let r1 = siguiente_revision(&mut sub).await.expect("la primera abre");
    assert_eq!(r1.total, 1);

    // Se confirma la petición 2 y se descarta la revisión 1.
    h.dispatch(UiAction::DialogInput {
        id: id2,
        text: "otra cosa".to_owned(),
    })
    .await
    .expect("host vivo");
    h.dispatch(UiAction::Dialog {
        id: id2,
        choice: "confirm".to_owned(),
        secret: None,
    })
    .await
    .expect("host vivo");
    h.dispatch(tecla("Escape")).await.expect("host vivo");

    // Y la petición 2 sigue viva. La señal que NO se puede confundir con un
    // parche rezagado de la revisión 1 es que el core reciba un SEGUNDO
    // veredicto: solo lo pide un plan que llegó y se abrió.
    anotados(&backend, "el segundo veredicto", 2, |f| {
        f.veredictos_pedidos.lock().expect("veredictos").clone()
    })
    .await;
    assert_eq!(
        backend.instrucciones.lock().expect("instrucciones").len(),
        2
    );
}

/// La primera tecla que llega a la revisión solo la RECONOCE.
///
/// La pantalla se abre sola, decenas de segundos después del gesto que la
/// pidió, y se queda el teclado. Sin este paso, la `y` de quien estaba
/// tecleando `yes.txt` en el filtro rápido aprobaba el renombrado del
/// directorio entero.
#[tokio::test]
async fn la_primera_tecla_solo_reconoce_la_revision() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }

    h.dispatch(tecla("y")).await.expect("host vivo");
    asentar().await;
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "la tecla que venía en camino no aprueba nada"
    );
    h.dispatch(tecla("y")).await.expect("host vivo");
    anotados(&backend, "el lote que aprueba la segunda tecla", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(
        backend.lotes.lock().expect("lotes").len(),
        1,
        "la segunda sí: ya es una respuesta"
    );
}

/// `Escape` NO necesita reconocimiento: descartar es seguro en los dos
/// estados, y quien no quiere esto tiene que poder quitárselo de encima a la
/// primera.
#[tokio::test]
async fn escape_descarta_la_revision_a_la_primera() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.ai_rename.is_none(), "se fue a la primera");
}

/// Un acorde CON modificador no es una respuesta a esta pantalla.
#[tokio::test]
async fn un_acorde_con_modificador_no_aprueba_el_plan() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");
    // Reconocida, para que lo único que quede en pie sea el modificador.
    h.dispatch(tecla("j")).await.expect("host vivo");

    for (ctrl, shift) in [(true, false), (false, false)] {
        let ack = h
            .dispatch(UiAction::Key(norte_ui_host::keys::KeyInput {
                key: "y".to_owned(),
                ctrl,
                alt: false,
                shift,
                meta: true,
            }))
            .await
            .expect("host vivo");
        assert_eq!(
            ack,
            ActionAck::Unavailable {
                reason_key: "host-key-unmapped".to_owned()
            },
            "ctrl={ctrl}: {ack:?}"
        );
    }
    asentar().await;
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// No se aprueba un plan que no se ha recorrido ENTERO.
///
/// La ventana son cinco parejas de hasta doscientas cincuenta y seis: sin
/// esto, la pareja doscientos se ejecutaba sin que nadie la hubiera pintado
/// jamás, y la revisión es toda la defensa que hay contra un plan que un
/// modelo escribió a partir de nombres que controla quien escribe en el
/// directorio.
#[tokio::test]
async fn no_se_aprueba_un_plan_sin_recorrerlo_entero() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, Some(veredicto_ok(&refs)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if !v.status.is_empty() && v.total == 12 && v.more_note.contains("12") {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    assert!(!v.seen_all, "todavía no se ha visto entero");
    assert!(!v.confirmable, "y por eso no se puede aprobar");

    // Reconocer, e intentar aprobar sin haber bajado.
    h.dispatch(tecla("y")).await.expect("host vivo");
    let ack = h.dispatch(tecla("y")).await.expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-plan-unseen".to_owned()
        },
        "{ack:?}"
    );

    // Se recorre hasta el final y ya sí.
    for _ in 0..6 {
        h.dispatch(tecla("PageDown")).await.expect("host vivo");
    }
    asentar().await;
    h.dispatch(tecla("y")).await.expect("host vivo");
    anotados(&backend, "el lote aprobado", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(backend.lotes.lock().expect("lotes").len(), 1);
}

/// Un nombre alterado FUERA de la ventana también se dice.
#[tokio::test]
async fn un_nombre_alterado_que_no_se_ve_tambien_se_dice() {
    let hostil =
        String::from_utf8(hostil("control_escape")).unwrap_or_else(|_| "\u{1b}x".to_owned());
    let mut pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    // En la posición ONCE: fuera de la primera ventana de cinco.
    pares[11].1 = hostil;
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert!(
        r.pairs.iter().all(|p| !p.to.hostile),
        "ninguna de las visibles está alterada"
    );
    assert!(
        r.hidden_hostile,
        "y aun así se dice que hay una que no se ve: {r:?}"
    );
}

/// La línea de «cuánto se ve» va TRADUCIDA, no como un patrón sin sustituir.
#[tokio::test]
async fn la_linea_de_cuanto_se_ve_va_traducida() {
    let pares: Vec<(String, String)> = (0..12)
        .map(|i| (format!("a{i:02}.mkv"), format!("b{i:02}.mkv")))
        .collect();
    let refs: Vec<(&str, &str)> = pares
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let backend = falso_con_plan(&refs, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    assert!(
        !r.more_note.contains('$') && !r.more_note.contains('{'),
        "sin patrones sin sustituir: {}",
        r.more_note
    );
    assert!(
        r.more_note.contains("12"),
        "y con el total: {}",
        r.more_note
    );
}

/// Crear un directorio tampoco escribe el carácter que puso la pantalla.
#[tokio::test]
async fn crear_un_directorio_con_fffd_se_rechaza() {
    let backend = arbol();
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F7")).await.expect("host vivo");
    let id = siguientes_dialogos(&mut sub).await[0].id;
    h.dispatch(UiAction::DialogInput {
        id,
        text: "caf\u{FFFD}".to_owned(),
    })
    .await
    .expect("host vivo");
    let ack = h
        .dispatch(UiAction::Dialog {
            id,
            choice: "confirm".to_owned(),
            secret: None,
        })
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "msg-transfer-name-fffd".to_owned()
        },
        "{ack:?}"
    );
    asentar().await;
    assert!(
        backend.creados.lock().expect("creados").is_empty(),
        "no se crea un directorio con el U+FFFD que inventó la pantalla"
    );
}

/// `Enter` NO aprueba el plan.
///
/// Rompe la paridad con el TUI a propósito: allí el plan lo abre una tecla del
/// lector y la siguiente es una respuesta. Aquí la pantalla se abre sola
/// decenas de segundos después, y `Enter` es justo la tecla con la que se
/// estaba recorriendo el árbol mientras el modelo pensaba — dos seguidos
/// entrando en directorios anidados son normales.
#[tokio::test]
async fn enter_no_aprueba_el_plan() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    for _ in 0..3 {
        h.dispatch(tecla("Enter")).await.expect("host vivo");
    }
    asentar().await;
    assert!(
        backend.lotes.lock().expect("lotes").is_empty(),
        "ningún Enter aprueba un lote"
    );
}

/// Un clic en el botón SÍ contesta a la primera: es un gesto dirigido a esta
/// pantalla, no una tecla que iba a otro sitio.
#[tokio::test]
async fn el_boton_aprueba_sin_reconocimiento_previo() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let mut v = siguiente_revision(&mut sub).await.expect("abre");
    for _ in 0..40 {
        if v.confirmable {
            break;
        }
        v = siguiente_revision(&mut sub).await.expect("sigue abierta");
    }
    let ack = h
        .dispatch(UiAction::AiRenameDecide { approve: true })
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    anotados(&backend, "el lote que aprueba el botón", 1, |f| {
        f.lotes.lock().expect("lotes").clone()
    })
    .await;
    assert_eq!(backend.lotes.lock().expect("lotes").len(), 1);
}

/// Y descartar con el botón cierra sin aplicar nada.
#[tokio::test]
async fn el_boton_de_descartar_cierra_sin_aplicar() {
    let pares = [("ep1.mkv", "ep01.mkv")];
    let backend = falso_con_plan(&pares, Some(veredicto_ok(&pares)));
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    siguiente_revision(&mut sub).await.expect("abre");
    h.dispatch(UiAction::AiRenameDecide { approve: false })
        .await
        .expect("host vivo");
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert!(foto.ai_rename.is_none());
    assert!(backend.lotes.lock().expect("lotes").is_empty());
}

/// Un nombre de plan que EMPIEZA por la flecha no puede fingir ser el destino
/// de otra pareja.
///
/// Fixture `arrow_leading_row_spoof` del corpus: `arrow_join_spoof` pone la
/// flecha en medio y falsifica UNA pareja; esta la pone al principio y
/// falsifica el PAPEL de la fila. El papel lo lleva el campo —`from` o `to`—,
/// no el texto, así que el host manda los dos por separado y el renderer los
/// pone en elementos distintos.
#[tokio::test]
async fn un_nombre_que_empieza_por_la_flecha_no_finge_ser_un_destino() {
    let bytes = hostil("arrow_leading_row_spoof");
    let trampa = String::from_utf8(bytes).expect("el del corpus es UTF-8");
    let pares = [(trampa.as_str(), "ep02.mkv")];
    let backend = falso_con_plan(&pares, None);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    pedir_plan(&h, &mut sub).await;
    let r = siguiente_revision(&mut sub).await.expect("abre");
    // El nombre viaja ENTERO y en el campo que le toca: la flecha que lleva
    // dentro no lo convierte en un destino, porque el papel no está en el
    // texto.
    assert!(
        r.pairs[0].from.text.starts_with('\u{2192}'),
        "el nombre real empieza por la flecha: {:?}",
        r.pairs[0].from
    );
    assert_eq!(r.pairs[0].to.text, "ep02.mkv");
    assert_eq!(r.pairs.len(), 1, "una pareja, no dos: {:?}", r.pairs);
}

/// Un nombre cuya proyección NO cabe en pantalla no se puede editar aquí, y
/// se dice.
///
/// El recorte le pega una elipsis, y `…` es un carácter legal en un nombre:
/// ni se enmascara ni se marca. Editar el campo y confirmar escribiría el
/// recorte en el disco como parte del nombre. Fixture
/// `display_expansion_over_clamp`.
#[tokio::test]
async fn un_nombre_que_no_cabe_en_pantalla_no_se_edita() {
    let bytes = hostil("display_expansion_over_clamp");
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(bytes, false)]);
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let ack = h
        .dispatch(tecla_mod("F6", false, true))
        .await
        .expect("host vivo");
    assert_eq!(
        ack,
        ActionAck::Unavailable {
            reason_key: "host-name-not-editable".to_owned()
        },
        "{ack:?}"
    );
}

/// Inyecta una task AJENA con su propio cancelador, y devuelve por dónde
/// mandarle progreso.
///
/// Cada una lleva un cancelador que apunta SU id: un contador compartido dice
/// que se canceló algo, no CUÁL, y «cuál» es justo lo que un tablero con
/// cursor tiene que acertar.
pub(super) fn inyectar_task(
    tx: &tokio::sync::mpsc::UnboundedSender<norte_ui_host::backend::HostTask>,
    id: u64,
    canceladas: &Arc<std::sync::Mutex<Vec<u64>>>,
) -> tokio::sync::watch::Sender<norte_proto::TaskProgress> {
    let progreso = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(id),
        kind: norte_proto::TaskKind::Copy,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (ptx, prx) = tokio::sync::watch::channel(progreso);
    let canceladas = Arc::clone(canceladas);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(id),
        progress: prx,
        cancel: Arc::new(move || canceladas.lock().expect("canceladas").push(id)),
        foreign: true,
    })
    .expect("el host escucha");
    ptx
}

/// Espera el siguiente aviso con clave, sea cual sea.
pub(super) async fn siguiente_aviso(sub: &mut norte_ui_host::controller::UiSubscription) -> String {
    for _ in 0..40 {
        match tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("llega")
            .expect("el host sigue vivo")
        {
            Update::Message(m) => {
                if let UiUpdate::Notice(UiNotice::Message { key, .. }) = &m.payload {
                    return key.clone();
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no llegó ningún aviso");
}

/// `Ctrl+K` para la task viva: hasta ahora el catálogo ataba la tecla y el
/// host respondía `NotHere`, así que una copia lanzada desde la ventana solo
/// se podía parar matando la ventana.
#[tokio::test]
async fn la_tecla_de_cancelar_para_la_task_viva() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _p = inyectar_task(&tx, 11, &canceladas);
    siguientes_tasks(&mut sub).await;

    let ack = h
        .dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![11],
        "se le pidió parar a la task viva"
    );
    assert_eq!(siguiente_aviso(&mut sub).await, "msg-cancelling");
}

/// Sin nada en marcha, cancelar no es un error ni un silencio: se dice.
#[tokio::test]
async fn cancelar_sin_tasks_lo_dice() {
    let (h, _snap) = host_arbol(arbol()).await;
    let mut sub = h.subscribe();
    let ack = h
        .dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert!(matches!(ack, ActionAck::Applied { .. }), "{ack:?}");
    assert_eq!(siguiente_aviso(&mut sub).await, "msg-no-tasks");
}

/// Una task ya TERMINADA sigue en el tablero, y cancelarla no es cancelar
/// nada: se busca una viva, y si no la hay se dice.
#[tokio::test]
async fn una_task_terminada_no_es_la_que_se_cancela() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let p = inyectar_task(&tx, 11, &canceladas);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    siguientes_tasks(&mut sub).await;

    h.dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert!(
        canceladas.lock().expect("canceladas").is_empty(),
        "a una task terminada no se le pide parar"
    );
    assert_eq!(siguiente_aviso(&mut sub).await, "msg-no-tasks");
}

/// Con el panel de procesos enfocado se cancela la del CURSOR, no la última.
///
/// Es la misma regla que el panel ya tenía para moverse: si la lista que se
/// ve tiene cursor y la tecla cancela otra cosa, el tablero pinta una
/// selección que no manda.
#[tokio::test]
async fn con_el_panel_enfocado_se_cancela_la_del_cursor() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_con_layout(Arc::new(falso), "full", (200, 60)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let _a = inyectar_task(&tx, 11, &canceladas);
    let _b = inyectar_task(&tx, 12, &canceladas);
    // Las dos en el tablero antes de tocar el cursor.
    for _ in 0..2 {
        if siguientes_tasks(&mut sub).await.len() == 2 {
            break;
        }
    }

    h.dispatch(UiAction::FocusSlot { slot_id: 7 })
        .await
        .expect("host vivo");
    // El tablero va por id, así que la segunda fila es la 12.
    h.dispatch(tecla("Down")).await.expect("host vivo");
    h.dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![12],
        "la del cursor, no la última"
    );
}

/// Espera el siguiente PARCHE de tablero, con su cursor.
///
/// Solo el parche: la foto entera no vale para lo que este test mira, que es
/// justo lo que el renderer sabe cuando NADIE le manda una foto.
pub(super) async fn siguiente_parche_de_tablero(
    sub: &mut norte_ui_host::UiSubscription,
) -> (Vec<norte_ui_host::dto::TaskView>, Option<u64>) {
    for _ in 0..20 {
        let siguiente = tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("un parche de tablero, no un cuelgue")
            .expect("el host sigue vivo");
        if let Update::Message(m) = siguiente
            && let UiUpdate::Patch(p) = &m.payload
        {
            for c in &p.changes {
                if let norte_ui_host::dto::ViewChange::Tasks { tasks, cursor } = c {
                    return (tasks.clone(), *cursor);
                }
            }
        }
    }
    panic!("no llegó ningún parche de tablero");
}

/// Una task que CADUCA se lleva su fila, y el cursor del panel viaja con ella.
///
/// El tablero se encoge solo —una terminada se va a los diez segundos— y eso
/// desplaza el resto. Dos averías, y el test cubre las dos:
///
/// - El cursor viajaba únicamente en la foto entera, así que el renderer se
///   quedaba resaltando la fila N mientras la tecla de cancelar actuaba sobre
///   la que el host tiene acotada.
/// - Y lo que se guardaba era la POSICIÓN. Cuando la que se va está ENCIMA de
///   la elegida, acotar no basta: la fila 1 pasa a nombrar otra tarea sin que
///   el lector toque nada, y cancelar para una copia que nadie eligió.
///
/// Reloj VIRTUAL, y se adelanta A MANO por lo mismo que en
/// [`una_task_terminada_se_va_del_tablero_sola`]: el salto automático de tokio
/// va al temporizador más cercano, que aquí sería el plazo de los ayudantes.
#[tokio::test(start_paused = true)]
async fn una_task_que_caduca_arrastra_el_cursor_del_panel() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let (h, _snap) = host_con_layout(Arc::new(falso), "full", (200, 60)).await;
    let mut sub = h.subscribe();
    let canceladas = Arc::new(std::sync::Mutex::new(Vec::new()));
    let primera = inyectar_task(&tx, 11, &canceladas);
    let _b = inyectar_task(&tx, 12, &canceladas);
    let _c = inyectar_task(&tx, 13, &canceladas);
    let _d = inyectar_task(&tx, 14, &canceladas);
    for _ in 0..5 {
        if siguientes_tasks(&mut sub).await.len() == 4 {
            break;
        }
    }

    // El cursor en la SEGUNDA fila de cuatro, que es la task 12. Ni la
    // primera ni la última, y ahí está el filo: la que va a caducar queda
    // ENCIMA, así que una implementación que guarde la POSICIÓN y la acote
    // deja el 1 —o sea la task 13— y una que guarde la IDENTIDAD baja al 0
    // con la 12. Con el cursor en la última fila las dos dan lo mismo y el
    // test no distinguiría nada.
    h.dispatch(UiAction::FocusSlot { slot_id: 7 })
        .await
        .expect("host vivo");
    h.dispatch(tecla("Down")).await.expect("host vivo");

    // La primera termina y, diez segundos después, se va del tablero.
    primera.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    for _ in 0..4 {
        if siguientes_tasks(&mut sub)
            .await
            .iter()
            .any(|t| t.state == norte_ui_host::dto::TaskStateView::Done)
        {
            break;
        }
    }
    tokio::time::advance(std::time::Duration::from_secs(11)).await;
    for _ in 0..4 {
        tokio::task::yield_now().await;
    }

    let mut visto = None;
    for _ in 0..5 {
        let (tasks, cursor) = siguiente_parche_de_tablero(&mut sub).await;
        if tasks.len() == 3 {
            visto = Some(cursor);
            break;
        }
    }
    assert_eq!(
        visto,
        Some(Some(0)),
        "la elegida sigue siendo la 12, que ahora es la primera fila: el parche \
         que quita la fila tiene que decir dónde queda el cursor"
    );

    // Y lo que se cancelaría es esa misma. El resalte y la tecla no pueden
    // apuntar a filas distintas, que es la avería entera; con una posición
    // acotada aquí se pararía la 13.
    h.dispatch(tecla_mod("k", true, false))
        .await
        .expect("host vivo");
    assert_eq!(
        *canceladas.lock().expect("canceladas"),
        vec![12],
        "la resaltada y la que para son la misma"
    );
}

/// Igual que [`inyectar_task`], pero eligiendo la CLASE: el informe de un
/// lote solo se pide para un lote.
pub(super) fn inyectar_task_de(
    tx: &tokio::sync::mpsc::UnboundedSender<norte_ui_host::backend::HostTask>,
    id: u64,
    kind: norte_proto::TaskKind,
) -> tokio::sync::watch::Sender<norte_proto::TaskProgress> {
    let progreso = norte_proto::TaskProgress {
        task_id: norte_proto::TaskId::new(id),
        kind,
        state: norte_proto::TaskState::Running,
        bytes_done: 0,
        bytes_total: None,
        entries_done: 0,
        entries_total: None,
        current: None,
        unreadable: None,
        unvisited: None,
    };
    let (ptx, prx) = tokio::sync::watch::channel(progreso);
    tx.send(norte_ui_host::backend::HostTask {
        id: norte_proto::TaskId::new(id),
        progress: prx,
        cancel: Arc::new(|| {}),
        foreign: false,
    })
    .expect("el host escucha");
    ptx
}

/// Un informe limpio: N aplicados y nada más.
pub(super) fn informe_limpio(n: u64) -> norte_proto::methods::FsRenameBatchReportResult {
    norte_proto::methods::FsRenameBatchReportResult {
        applied: n,
        rolled_back: 0,
        failed_pair: None,
        stuck: None,
        uncertain: None,
        compensations_lost: 0,
    }
}

/// Espera a que el tablero traiga una task con `detail` puesto.
pub(super) async fn detalle_de_task(sub: &mut norte_ui_host::controller::UiSubscription) -> String {
    for _ in 0..40 {
        let tasks = siguientes_tasks(sub).await;
        if let Some(d) = tasks.first().and_then(|t| t.detail.clone()) {
            return d;
        }
    }
    panic!("ninguna task trajo detalle");
}

/// Un lote que termina PIDE su informe: es la única señal de que el
/// directorio se quedó a medias, y hasta ahora no lo pedía nadie (#272).
#[tokio::test]
async fn un_lote_terminado_pide_su_informe() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") = Some(informe_limpio(3));
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 31, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);

    let detalle = detalle_de_task(&mut sub).await;
    assert_eq!(
        *backend.informes_pedidos.lock().expect("informes"),
        vec![31],
        "se pidió el informe del lote"
    );
    assert!(detalle.contains('3'), "el tablero dice cuántos: {detalle}");
    // Un lote limpio no interrumpe: no hay nada que decidir ni que buscar.
    assert!(
        !hubo_dialogos(&mut sub).await,
        "un lote limpio no abre nada"
    );
}

/// `true` si en lo que queda por leer llega algún diálogo.
pub(super) async fn hubo_dialogos(sub: &mut norte_ui_host::controller::UiSubscription) -> bool {
    while let Ok(Some(u)) =
        tokio::time::timeout(std::time::Duration::from_millis(150), sub.recv()).await
    {
        if let Update::Message(m) = u
            && let UiUpdate::Patch(p) = &m.payload
            && p.changes
                .iter()
                .any(|c| matches!(c, norte_ui_host::dto::ViewChange::Dialogs { .. }))
        {
            return true;
        }
    }
    false
}

/// Una copia que termina NO pide informe de lote: el informe es de los lotes.
#[tokio::test]
async fn una_copia_no_pide_informe_de_lote() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 32, norte_proto::TaskKind::Copy);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| p.state = norte_proto::TaskState::Completed);
    siguientes_tasks(&mut sub).await;
    asentar().await;
    assert!(
        backend
            .informes_pedidos
            .lock()
            .expect("informes")
            .is_empty()
    );
}

/// Un lote ATASCADO abre una superficie que lo dice, y dice CÓMO SE LLAMA
/// AHORA el fichero: sin ese nombre, «se quedó a medias» no se puede actuar.
#[tokio::test]
async fn un_lote_atascado_lo_dice_y_da_el_nombre_de_ahora() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    *falso.informe.lock().expect("informe") =
        Some(norte_proto::methods::FsRenameBatchReportResult {
            applied: 4,
            rolled_back: 2,
            failed_pair: Some(2),
            stuck: Some(norte_proto::methods::RenameStuckStep {
                from: VPath::parse("mem:///casa/viejo.txt").expect("vpath"),
                to: VPath::parse("mem:///casa/nuevo.txt").expect("vpath"),
                pair_index: 2,
                error: norte_proto::Error::Io { retryable: false },
                journalled: true,
                still_applied: 2,
            }),
            uncertain: None,
            compensations_lost: 1,
        });
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 33, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Io { retryable: false },
        };
    });

    let dialogos = siguientes_dialogos(&mut sub).await;
    let cuerpo: String = dialogos[0]
        .body
        .iter()
        .map(|l| l.text.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        cuerpo.contains("nuevo.txt"),
        "dice cómo se llama AHORA: {cuerpo}"
    );
    // Y la marca de compensaciones perdidas no se calla: un undo de sesión se
    // va a parar justo ahí.
    assert!(cuerpo.contains('1'), "{cuerpo}");
}

/// Un daemon que NO sabe informar de un lote fallido no se degrada en
/// silencio: se dice que el desenlace se quedó sin comprobar.
#[tokio::test]
async fn un_informe_que_no_se_puede_pedir_se_dice() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.ajenas.lock().expect("ajenas") = Some(rx);
    // Sin informe: el falso contesta `Unsupported`.
    let backend = Arc::new(falso);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();
    let p = inyectar_task_de(&tx, 34, norte_proto::TaskKind::RenameBatch);
    siguientes_tasks(&mut sub).await;
    p.send_modify(|p| {
        p.state = norte_proto::TaskState::Failed {
            error: norte_proto::Error::Io { retryable: false },
        };
    });

    let dialogos = siguientes_dialogos(&mut sub).await;
    assert_eq!(dialogos[0].title_key, "modal-batch-report-title");
    // Y dice EXACTAMENTE que el daemon no sabe informar: «no se pudo pedir»
    // y «este daemon no sabe» son dos cosas distintas, y confundirlas es
    // degradar en silencio con más palabras.
    assert_eq!(
        dialogos[0].body[0].text,
        norte_i18n::t_in(norte_i18n::Lang::Es, "modal-batch-unsupported"),
        "{:?}",
        dialogos[0].body
    );
}

/// Espera el siguiente estado de la barra que traiga avisos persistentes.
pub(super) async fn siguientes_banners(
    sub: &mut norte_ui_host::controller::UiSubscription,
) -> Vec<norte_ui_host::dto::BannerView> {
    for _ in 0..40 {
        match tokio::time::timeout(ESPERA_MAX, sub.recv())
            .await
            .expect("llega")
            .expect("el host sigue vivo")
        {
            Update::Message(m) => {
                if let UiUpdate::Patch(p) = &m.payload {
                    for c in &p.changes {
                        if let norte_ui_host::dto::ViewChange::Status(s) = c
                            && !s.banners.is_empty()
                        {
                            return s.banners.clone();
                        }
                    }
                }
            }
            Update::Lagged => {}
        }
    }
    panic!("no llegó ningún aviso persistente");
}

/// Una sesión que viaja SIN cifrar deja un aviso persistente que la NOMBRA.
///
/// Un mensaje efímero no vale: lo borra la siguiente tecla, y esto es un
/// hecho de toda la sesión. La ventana lo pintaba de ninguna manera —el
/// canal existía en el SDK y el host no lo tomaba— así que un FTP en claro
/// se leía igual que un SFTP.
#[tokio::test]
async fn una_sesion_en_claro_deja_aviso_persistente() {
    let falso = arbol_como_falso();
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    *falso.degradadas.lock().expect("degradadas") = Some(rx);
    let (h, _snap) = host_arbol(Arc::new(falso)).await;
    let mut sub = h.subscribe();
    tx.send(norte_proto::methods::ConnectionDegraded {
        scheme: "ftp".to_owned(),
        host: "archivo.example".to_owned(),
        reason: "ftp-plaintext".to_owned(),
        detail: None,
    })
    .expect("el host escucha");

    let banners = siguientes_banners(&mut sub).await;
    assert!(
        banners.iter().any(|b| b
            .subject
            .as_ref()
            .is_some_and(|s| s.host == "archivo.example")),
        "el aviso nombra la conexión, en su propio campo: {banners:?}"
    );
}
