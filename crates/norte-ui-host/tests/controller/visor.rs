use super::*;

// ---------------------------------------------------------------------------
// El visor (fase 4, tarea 4.3).
// ---------------------------------------------------------------------------

/// Bytes que `norte_encoding::detect` clasifica como BINARIO y que empiezan
/// por la firma PNG: la firma sola son ocho bytes sin ningún NUL y la
/// heurística los toma por texto, con lo que `is_image()` saldría `false`.
/// Mismo molde que `png_bytes_binarios` de la TUI.
fn png_binario() -> Vec<u8> {
    let mut bytes = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
    bytes.resize(40, 0);
    bytes
}

/// **Pasar fotos pasa fotos, y el cursor va contigo.**
///
/// El listado va ordenado, así que `b.txt` queda ENTRE las dos imágenes: es
/// justo la fila que `viewer.next` tiene que saltarse. Y al final del carrete
/// se dice que no hay más en vez de volver a la primera.
#[tokio::test]
async fn la_hermana_siguiente_salta_el_texto_de_en_medio() {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"a.png".to_vec(), false),
            (b"b.txt".to_vec(), false),
            (b"c.png".to_vec(), false),
        ],
    );
    f.contenido
        .insert("mem:///casa/a.png".to_owned(), png_binario());
    f.contenido
        .insert("mem:///casa/c.png".to_owned(), png_binario());
    f.contenido
        .insert("mem:///casa/b.txt".to_owned(), b"texto\n".to_vec());
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let v = siguiente_visor(&mut sub).await.expect("el visor abre");
    assert!(v.path_display.ends_with("a.png"), "abre la primera foto");

    // F9 = `viewer.next`: la SIGUIENTE imagen, no el texto de en medio.
    h.dispatch(tecla("F9")).await.expect("host vivo");
    let v = siguiente_visor(&mut sub).await.expect("el visor abre otra");
    assert!(
        v.path_display.ends_with("c.png"),
        "salta b.txt: {}",
        v.path_display
    );

    // Y desde la última no hay más: no da la vuelta a la primera.
    let ack = h.dispatch(tecla("F9")).await.expect("host vivo");
    assert!(
        matches!(&ack, ActionAck::Unavailable { reason_key } if reason_key == "host-no-sibling"),
        "al final del carrete se dice que no hay más: {ack:?}"
    );

    // F8 vuelve, con la misma regla.
    h.dispatch(tecla("F8")).await.expect("host vivo");
    let v = siguiente_visor(&mut sub).await.expect("el visor vuelve");
    assert!(
        v.path_display.ends_with("a.png"),
        "hacia atrás también salta el texto: {}",
        v.path_display
    );
}

/// F3 sobre un fichero lo ABRE: se lee una cabecera acotada, se decodifica
/// con la detección compartida y lo que viaja son líneas ya saneadas.
#[tokio::test]
async fn ver_un_fichero_lo_decodifica_y_lo_pinta() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/notas.txt".to_owned(),
        b"primera\nsegunda\ntercera\n".to_vec(),
    );
    let backend = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&backend)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let v = siguiente_visor(&mut sub)
        .await
        .expect("el visor está abierto");
    assert!(v.path_display.ends_with("notas.txt"));
    assert_eq!(v.total_rows, 3, "tres líneas");
    assert!(
        v.lines.iter().any(|l| l == "primera"),
        "y el texto llega decodificado: {:?}",
        v.lines
    );
    assert!(!v.hex, "un texto no se enseña en hexadecimal");
    assert_eq!(v.encoding.to_ascii_uppercase(), "UTF-8");
}

/// Con el visor abierto, las teclas son SUYAS: el mismo mapa de la pantalla
/// `viewer` que usa el TUI, no un segundo keymap escrito aquí.
#[tokio::test]
async fn con_el_visor_abierto_las_teclas_son_del_visor() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    let mut cuerpo = String::new();
    for i in 0..200 {
        use std::fmt::Write as _;
        let _ = writeln!(cuerpo, "linea {i}");
    }
    let cuerpo = cuerpo.into_bytes();
    f.contenido
        .insert("mem:///casa/notas.txt".to_owned(), cuerpo);
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F3")).await.expect("host vivo");
    let abierto = siguiente_visor(&mut sub).await.expect("visor abierto");
    assert_eq!(abierto.first_line, 0);

    // `down` en el visor DESPLAZA el visor; no mueve el cursor del listado.
    h.dispatch(tecla("ArrowDown")).await.expect("host vivo");
    let bajado = siguiente_visor(&mut sub).await.expect("visor abierto");
    assert_eq!(bajado.first_line, 1);

    // Y `esc` lo cierra.
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        siguiente_visor(&mut sub).await.is_none(),
        "el visor se cierra"
    );

    // El listado de debajo no se movió, y para verlo hace falta una foto:
    // el visor viaja en parches justo para no mandarla en cada tecla.
    h.dispatch(UiAction::Resync).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    assert_eq!(listado(&foto).cursor, Some(RowKey(0)));
}

/// Un binario no se pinta como si fuera texto: se enseña en hexadecimal, y
/// lo decide la capa compartida por el CONTENIDO, no por la extensión.
#[tokio::test]
async fn un_binario_se_enseña_en_hexadecimal() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"raro.txt".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/raro.txt".to_owned(),
        vec![0x00, 0x01, 0x02, 0xff, 0xfe, 0x00, 0x03],
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F3")).await.expect("host vivo");
    let foto = siguiente_foto(&mut sub).await;
    let v = foto.viewer.as_ref().expect("visor");
    assert!(
        v.hex,
        "un binario entra en hexadecimal aunque se llame .txt"
    );
}

// ---------------------------------------------------------------------------
// La preview de un plugin en el visor (tarea 4.3).
// ---------------------------------------------------------------------------

/// Una preview con estilo, como la devolvería un previewer.
pub(super) fn preview_de(
    plugin: &str,
    lineas: &[&str],
    lossy: bool,
) -> norte_proto::methods::PluginPreviewStyled {
    norte_proto::methods::PluginPreviewStyled {
        plugin_id: "acme.pdf".to_owned(),
        plugin_name: plugin.to_owned(),
        lines: lineas
            .iter()
            .map(|l| {
                vec![norte_proto::methods::SpanWire {
                    text: (*l).to_owned(),
                    role: Some("info".to_owned()),
                    fg: None,
                    bg: None,
                }]
            })
            .collect(),
        lossy,
    }
}

/// ADR 0141: una imagen que la ventana pinta SOLA no pasa por ningún
/// plugin. Antes, con un previewer de imágenes instalado, su vista con
/// estilo (arte ANSI) ganaba, la ventana se quedaba sin imagen propia y
/// además pedía la miniatura: dos plugins compilados por abrir una foto.
#[tokio::test]
async fn una_imagen_propia_no_pregunta_a_los_plugins() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/foto.png".to_owned(), png_de(640, 480, 4096));
    // Un previewer que casaría: no se le tiene que preguntar.
    f.previews.insert(
        "mem:///casa/foto.png".to_owned(),
        preview_de("Imagen ANSI", &["▀▀▀"], false),
    );
    let f = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F3")).await.expect("host vivo");
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(x) = siguiente_foto(&mut sub).await.viewer.clone() {
            v = Some(x);
            break;
        }
    }
    let v = v.expect("el visor abre");
    assert!(v.image.is_some(), "la ventana pinta la suya");
    assert!(
        v.preview_by.is_empty(),
        "sin vista de plugin: {:?}",
        v.preview_by
    );
    // Unas vueltas más para que un estilo tardío, si se hubiera pedido,
    // tuviera tiempo de llegar.
    for _ in 0..5 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(&mut sub).await;
    }
    assert!(
        f.anchos_de_preview.lock().expect("mutex").is_empty(),
        "no se preguntó a ningún previewer"
    );
}

/// Cuando un previewer aplica, el visor enseña LO SUYO y dice de quién es.
///
/// Un plugin puede enseñar cualquier cosa —ese es su trabajo: un PDF como
/// texto, un JSON formateado— así que quien mira tiene derecho a saber que no
/// está viendo los bytes del fichero.
#[tokio::test]
async fn el_visor_ensena_la_preview_de_un_plugin_y_dice_de_quien_es() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"informe.pdf".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/informe.pdf".to_owned(),
        b"%PDF-1.7 crudo".to_vec(),
    );
    f.previews.insert(
        "mem:///casa/informe.pdf".to_owned(),
        preview_de("PDF de ACME", &["Informe anual", "Página 1 de 12"], true),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    // La vista del plugin llega DESPUÉS de abrir (ADR 0141): el visor se
    // abre con la cruda y esta la sustituye. Se espera a ella.
    let mut visor = None;
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone()
            && !v.preview_by.is_empty()
        {
            visor = Some(v);
            break;
        }
    }
    let v = visor.expect("el visor abre con la vista del plugin");

    assert!(
        v.lines.iter().any(|l| l.contains("Informe anual")),
        "enseña lo del previewer: {:?}",
        v.lines
    );
    assert!(
        !v.lines.iter().any(|l| l.contains("%PDF")),
        "y NO los bytes crudos: enseñar las dos cosas sería el mismo fichero \
         dos veces — {:?}",
        v.lines
    );
    assert!(
        v.preview_by.contains("PDF de ACME"),
        "y dice de quién es lo que enseña: {:?}",
        v.preview_by
    );
    assert!(
        !v.preview_by.starts_with("viewer-plugin"),
        "traducido, no la clave: {:?}",
        v.preview_by
    );
    assert!(
        v.preview_lossy,
        "y que la decodificación que se le dio fue con pérdida: los `?` de su \
         salida vienen de ahí y no del fichero"
    );
}

/// La preview de un plugin llega a la ventana CON sus fragmentos (puente
/// 49): rol del tema en kebab, color propio en `#rrggbb`, texto ya
/// enmascarado. Hasta aquí `ViewerView` la aplanaba a `lines`, y la ventana
/// pintaba en gris lo que la TUI pintaba en color.
#[tokio::test]
async fn el_visor_lleva_los_fragmentos_de_la_preview() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"main.rs".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    f.previews.insert(
        "mem:///casa/main.rs".to_owned(),
        norte_proto::methods::PluginPreviewStyled {
            plugin_id: "acme.syntax".to_owned(),
            plugin_name: "Syntax".to_owned(),
            lines: vec![
                vec![
                    norte_proto::methods::SpanWire {
                        text: "fn".to_owned(),
                        role: Some("title".to_owned()),
                        fg: Some([255, 0, 0]),
                        bg: None,
                    },
                    norte_proto::methods::SpanWire {
                        text: " main".to_owned(),
                        role: None,
                        fg: Some([0, 128, 255]),
                        bg: Some([0, 0, 64]),
                    },
                    norte_proto::methods::SpanWire {
                        // Un rol que el tema no conoce degrada a plano, y un
                        // override bidi del plugin llega enmascarado.
                        text: "()\u{202e}{}".to_owned(),
                        role: Some("no-es-un-rol".to_owned()),
                        fg: None,
                        bg: None,
                    },
                ],
                vec![norte_proto::methods::SpanWire {
                    text: "plano".to_owned(),
                    role: None,
                    fg: None,
                    bg: None,
                }],
            ],
            lossy: false,
        },
    );
    let f = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let mut visor = None;
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone()
            && !v.styled.is_empty()
        {
            visor = Some(v);
            break;
        }
    }
    let v = visor.expect("el visor abre con la vista del plugin");

    // El ancho del visor viaja con la petición (0.66.0): es el viewport
    // con el que arrancó el host, no un `None` que deja elegir al guest.
    assert_eq!(
        f.anchos_de_preview.lock().expect("mutex").as_slice(),
        &[Some(120)],
        "una petición, con el ancho del viewport"
    );

    assert_eq!(v.styled.len(), 2, "una entrada por fila: {:?}", v.styled);
    assert_eq!(
        v.styled.len(),
        v.lines.len(),
        "las mismas filas que `lines`"
    );
    let primera = &v.styled[0];
    assert_eq!(primera.len(), 3);
    assert_eq!(primera[0].text, "fn");
    assert_eq!(primera[0].role.as_deref(), Some("title"));
    assert_eq!(primera[0].fg.as_deref(), Some("#ff0000"));
    assert_eq!(primera[1].role, None);
    assert_eq!(primera[1].fg.as_deref(), Some("#0080ff"));
    assert_eq!(
        primera[1].bg.as_deref(),
        Some("#000040"),
        "el fondo cruza (puente 50)"
    );
    assert_eq!(primera[0].bg, None);
    assert_eq!(primera[2].role, None, "un rol desconocido degrada a plano");
    assert!(
        !primera[2].text.contains('\u{202e}'),
        "el override bidi no cruza crudo: {:?}",
        primera[2].text
    );
    assert_eq!(v.styled[1][0].text, "plano");
    assert_eq!(
        v.lines[0],
        primera.iter().map(|s| s.text.as_str()).collect::<String>(),
        "`lines` es el mismo texto, aplanado"
    );
}

/// Lo que el renderer MIDIÓ del cuerpo del visor (puente 53) manda sobre el
/// viewport la próxima vez que se abre: el viewport cuenta el cromo, y una
/// imagen encogida a él se salía por la derecha.
#[tokio::test]
async fn el_visor_pide_el_ancho_que_midio_el_renderer() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"main.rs".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/main.rs".to_owned(), b"fn main() {}".to_vec());
    let f = Arc::new(f);
    let (h, _snap) = host_arbol(Arc::clone(&f)).await;
    let mut sub = h.subscribe();
    h.dispatch(tecla("F3")).await.expect("host vivo");
    assert_eq!(
        anchos_de_preview_tras(&h, &mut sub, &f, 1).await.as_slice(),
        &[Some(120)]
    );

    h.dispatch(UiAction::SetViewerCols { cols: 77 })
        .await
        .expect("host vivo");
    h.dispatch(tecla("Escape")).await.expect("host vivo");
    h.dispatch(tecla("F3")).await.expect("host vivo");
    assert_eq!(
        anchos_de_preview_tras(&h, &mut sub, &f, 2).await.as_slice(),
        &[Some(120), Some(77)],
        "la segunda apertura pide el ancho medido"
    );
}

/// Pide fotos hasta que el backend falso haya visto `n` peticiones de
/// preview con estilo, y devuelve los anchos que llevaban.
pub(super) async fn anchos_de_preview_tras(
    h: &UiHost,
    sub: &mut norte_ui_host::UiSubscription,
    f: &Falso,
    n: usize,
) -> Vec<Option<u32>> {
    let mut anchos = Vec::new();
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let _ = siguiente_foto(sub).await;
        anchos.clone_from(&f.anchos_de_preview.lock().expect("mutex"));
        if anchos.len() >= n {
            break;
        }
    }
    anchos
}

/// Un previewer que no aplica NO estorba: el visor enseña el fichero.
#[tokio::test]
async fn sin_previewer_el_visor_ensena_el_fichero() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"notas.txt".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/notas.txt".to_owned(),
        b"hola\nmundo\n".to_vec(),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() {
            assert!(v.lines.iter().any(|l| l.contains("hola")));
            assert!(
                v.preview_by.is_empty(),
                "sin plugin no se atribuye a nadie: {:?}",
                v.preview_by
            );
            return;
        }
    }
    panic!("el visor abre igual sin previewer");
}

/// El nombre de un previewer es texto de TERCERO y llega enmascarado.
#[tokio::test]
async fn el_nombre_del_previewer_llega_enmascarado() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"x.bin".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/x.bin".to_owned(), b"\x00\x01".to_vec());
    f.previews.insert(
        "mem:///casa/x.bin".to_owned(),
        preview_de("ACME\u{202e}gpj", &["contenido"], false),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() {
            assert!(
                !v.preview_by.contains('\u{202e}'),
                "el nombre del plugin va crudo: {:?}",
                v.preview_by
            );
            return;
        }
    }
    panic!("el visor abre");
}

// ---------------------------------------------------------------------------
// La imagen del visor (tarea 4.3, ADR 0069).
// ---------------------------------------------------------------------------

/// Un PNG cuya CABECERA declara `w`x`h`, con relleno hasta `bytes`.
pub(super) fn png_de(w: u32, h: u32, bytes: usize) -> Vec<u8> {
    let mut v = b"\x89PNG\r\n\x1a\n".to_vec();
    v.extend_from_slice(&[0, 0, 0, 13]);
    v.extend_from_slice(b"IHDR");
    v.extend_from_slice(&w.to_be_bytes());
    v.extend_from_slice(&h.to_be_bytes());
    v.resize(bytes.max(v.len()), 0);
    v
}

/// El visor abre la imagen: dice su formato y su tamaño DECLARADO, y sus
/// bytes NO viajan en la foto.
#[tokio::test]
async fn una_imagen_se_acepta_y_sus_bytes_van_aparte() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/foto.png".to_owned(), png_de(1920, 1080, 4096));
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let mut v = None;
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(x) = siguiente_foto(&mut sub).await.viewer.clone() {
            v = Some(x);
            break;
        }
    }
    let v = v.expect("el visor abre");
    let img = v.image.clone().expect("se reconoce la imagen");
    assert_eq!(img.format, "PNG", "por bytes MÁGICOS, no por la extensión");
    assert_eq!((img.width, img.height), (1920, 1080));
    assert!(v.image_refused.is_empty());

    // Los bytes NO están en la foto: ocho megas en el flujo de parches es un
    // mensaje que se reenvía entero en cada `Resync`.
    let foto = serde_json::to_string(&v).expect("serializa");
    assert!(
        foto.len() < 4096,
        "la vista del visor pesa {} bytes: los de la imagen se han colado",
        foto.len()
    );
    // Y se sirven por su propio camino.
    let bytes = h
        .image_bytes()
        .await
        .expect("host vivo")
        .expect("hay bytes");
    assert_eq!(bytes.len(), 4096);
}

/// Una cabecera que declara una BOMBA se rechaza, y se dice.
///
/// Un PNG de cuatro kilobytes puede declarar 60000×60000 —36 gigapíxeles— y
/// costarle gigabytes al decodificador. La cabecera se lee y se niega ANTES
/// de que nadie decodifique, que es la única defensa barata (ADR 0069).
#[tokio::test]
async fn una_cabecera_que_declara_una_bomba_se_rechaza() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"bomba.png".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/bomba.png".to_owned(),
        png_de(60000, 60000, 4096),
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() else {
            continue;
        };
        assert!(v.image.is_none(), "no se pinta");
        assert!(
            !v.image_refused.is_empty(),
            "y se DICE: caer al hexview en silencio parece norte roto, no \
             norte prudente"
        );
        assert!(
            !v.image_refused.starts_with("viewer-image"),
            "traducido, no la clave: {:?}",
            v.image_refused
        );
        assert!(
            h.image_bytes().await.expect("host vivo").is_none(),
            "y sus bytes no se sirven a nadie"
        );
        return;
    }
    panic!("el visor abre igual");
}

/// Una cabecera que no se entiende también se rechaza.
///
/// «No sé» tratado como «adelante» es la puerta que el presupuesto existe
/// para cerrar.
#[tokio::test]
async fn una_cabecera_que_no_se_entiende_se_rechaza() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"raro.png".to_vec(), false)]);
    // Firma PNG válida, pero el primer chunk NO es IHDR.
    let mut roto = b"\x89PNG\r\n\x1a\n".to_vec();
    roto.extend_from_slice(&[0, 0, 0, 13]);
    roto.extend_from_slice(b"iTXt");
    roto.resize(64, 0);
    f.contenido.insert("mem:///casa/raro.png".to_owned(), roto);
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        let Some(v) = siguiente_foto(&mut sub).await.viewer.clone() else {
            continue;
        };
        assert!(v.image.is_none());
        assert!(!v.image_refused.is_empty(), "se dice que no se entiende");
        return;
    }
    panic!("el visor abre igual");
}

/// Cerrar el visor SUELTA los bytes: son megas.
#[tokio::test]
async fn cerrar_el_visor_suelta_la_imagen() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"foto.png".to_vec(), false)]);
    f.contenido
        .insert("mem:///casa/foto.png".to_owned(), png_de(64, 64, 2048));
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    for _ in 0..20 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if siguiente_foto(&mut sub).await.viewer.is_some() {
            break;
        }
    }
    assert!(h.image_bytes().await.expect("host vivo").is_some());

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(
        h.image_bytes().await.expect("host vivo").is_none(),
        "un visor cerrado no retiene megas de imagen"
    );
}

/// Un fichero que el visor no sabe pintar como imagen, y un plugin de
/// miniaturas que sí (ADR 0107): el visor anuncia la miniatura como imagen,
/// dice de quién es, sirve sus bytes por el mismo canal que una imagen
/// propia, y los suelta al cerrar.
#[tokio::test]
async fn el_visor_ensena_la_miniatura_de_un_plugin_y_dice_de_quien_es() {
    let mut f = Falso::default();
    f.pon("mem:///casa", vec![(b"informe.pdf".to_vec(), false)]);
    f.contenido.insert(
        "mem:///casa/informe.pdf".to_owned(),
        b"%PDF-1.7 crudo".to_vec(),
    );
    f.thumbnails.insert(
        "mem:///casa/informe.pdf".to_owned(),
        norte_proto::methods::PluginThumbnail {
            plugin_id: "org.acme.thumbs".to_owned(),
            plugin_name: "Miniaturas ACME".to_owned(),
            mimetype: "image/png".to_owned(),
            bytes: b"\x89PNG\r\n\x1a\nno hace falta que sea real aqui".to_vec(),
            width: 120,
            height: 60,
        },
    );
    let (h, _snap) = host_arbol(Arc::new(f)).await;
    let mut sub = h.subscribe();

    h.dispatch(tecla("F3")).await.expect("host vivo");
    let mut visor = None;
    for _ in 0..40 {
        h.dispatch(UiAction::Resync).await.expect("host vivo");
        if let Some(v) = siguiente_foto(&mut sub).await.viewer.clone()
            && v.image.is_some()
        {
            visor = Some(v);
            break;
        }
    }
    let v = visor.expect("el visor abre con la miniatura");
    let img = v.image.expect("anuncia una imagen");
    assert_eq!(
        (img.format.as_str(), img.width, img.height),
        ("PNG", 120, 60)
    );
    assert!(
        v.preview_by.contains("Miniaturas ACME"),
        "y dice de quién es: {:?}",
        v.preview_by
    );
    assert_eq!(v.image_refused, "", "con miniatura no hay motivo que dar");
    let bytes = h
        .image_bytes()
        .await
        .expect("host vivo")
        .expect("los bytes");
    assert!(bytes.starts_with(b"\x89PNG"), "los de la miniatura");

    h.dispatch(tecla("Escape")).await.expect("host vivo");
    assert!(h.image_bytes().await.expect("host vivo").is_none());
}
