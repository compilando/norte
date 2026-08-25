//! La frontera de la webview, clavada (ADR 0066, decisión D11).
//!
//! Estas comprobaciones leen la CONFIGURACIÓN y los assets empaquetados, no el
//! runtime, y ese es justo el punto: una CSP relajada, una capacidad de más o
//! un `<script src="https://…">` colado en el bundle no se ven en ninguna
//! prueba de comportamiento — se ven aquí, en el diff, o no se ven nunca.

use std::path::{Path, PathBuf};

fn raiz() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn json(rel: &str) -> serde_json::Value {
    let p = raiz().join(rel);
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("no se pudo leer {}: {e}", p.display()));
    serde_json::from_str(&raw).expect("JSON válido")
}

/// La CSP no deja hueco: ni scripts remotos, ni `eval`, ni estilos en línea.
#[test]
fn la_csp_no_deja_puertas() {
    let cfg = json("tauri.conf.json");
    let csp = cfg["app"]["security"]["csp"]
        .as_str()
        .expect("la CSP está puesta");
    // El ÚNICO origen con esquema que se admite es el del propio IPC de
    // Tauri, que no es remoto: es como la webview le habla a este proceso.
    // Todo lo demás —un CDN, un websocket de desarrollo, un comodín— es una
    // puerta al exterior y no puede estar.
    let sin_ipc = csp.replace("http://ipc.localhost", "");
    for prohibido in [
        "'unsafe-inline'",
        "'unsafe-eval'",
        "http://",
        "https://",
        "ws://",
        "wss://",
        "*",
    ] {
        assert!(
            !sin_ipc.contains(prohibido),
            "la CSP no puede contener {prohibido}: {csp}"
        );
    }
    assert!(
        csp.starts_with("default-src 'none'"),
        "cerrada por defecto: {csp}"
    );
    for directiva in [
        "script-src 'self'",
        "object-src 'none'",
        "base-uri 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(directiva), "falta `{directiva}`: {csp}");
    }
    // Las imágenes: `blob:` SÍ, `data:` NO (ADR 0069).
    //
    // `blob:` no se puede fabricar desde el contenido —un blob URL existe
    // solo porque este documento lo creó— así que es una concesión más
    // estrecha que `data:`, que es una URL que cualquier cadena puede
    // formar. La diferencia importa aunque hoy este documento no pinte
    // markup ajeno: la CSP es del DOCUMENTO entero, no del elemento que
    // teníamos en mente.
    assert!(
        csp.contains("img-src 'self' blob:"),
        "las imágenes cruzan como blob (ADR 0069): {csp}"
    );
    assert!(
        !csp.contains("data:"),
        "`data:` no entra en la CSP sin cambiar el ADR 0069, que explica por \
         qué se eligió `blob:`: {csp}"
    );
}

/// La webview no tiene ni el objeto global de Tauri, ni protocolo de assets,
/// ni un servidor de desarrollo al que ir en producción.
#[test]
fn la_ventana_no_trae_nada_de_serie() {
    let cfg = json("tauri.conf.json");
    assert_eq!(
        cfg["app"]["withGlobalTauri"],
        serde_json::Value::Bool(false),
        "sin `window.__TAURI__`: lo que se puede llamar se importa, y está en la lista"
    );
    assert_eq!(
        cfg["app"]["security"]["assetProtocol"]["enable"],
        serde_json::Value::Bool(false),
        "sin protocolo de assets no hay forma de pedirle un fichero del disco"
    );
    assert!(
        cfg["build"]["devUrl"].is_null(),
        "un binario de producción no apunta a un servidor de desarrollo"
    );
    // Soltar SÍ significa algo desde #283 (ADR 0074), y lo que significa es
    // una pregunta: el drop llega al proceso —nunca a la webview, que no ve
    // las rutas— y abre la confirmación de copia. La afirmación se queda
    // porque el valor es una decisión, no un descuido: si alguien lo vuelve a
    // poner en `false` habrá borrado el gesto entero sin tocar una línea de
    // Rust.
    assert_eq!(
        cfg["app"]["windows"][0]["dragDropEnabled"],
        serde_json::Value::Bool(true),
        "soltar entra por el proceso y abre una confirmación (#283)"
    );
}

/// El fichero de capacidades concede LO MÍNIMO: escuchar eventos. Nada de
/// filesystem, shell, http, diálogo nativo ni control de ventana.
#[test]
fn las_capacidades_son_las_minimas() {
    let cap = json("capabilities/main.json");
    let permisos: Vec<&str> = cap["permissions"]
        .as_array()
        .expect("hay lista de permisos")
        .iter()
        .map(|p| p.as_str().expect("cada permiso es una cadena"))
        .collect();
    assert_eq!(
        permisos,
        vec!["core:event:allow-listen", "core:event:allow-unlisten"],
        "cualquier permiso de más es una decisión, y se ve aquí"
    );
    assert_eq!(
        cap["windows"].as_array().map(Vec::len),
        Some(1),
        "una ventana, la principal"
    );
}

/// Los comandos que el binario registra son EXACTAMENTE los declarados.
///
/// Se lee el `main.rs` a propósito: `generate_handler!` es un macro, así que
/// un comando nuevo no aparece en ninguna lista que se pueda comparar en
/// tiempo de compilación. Esto lo convierte en algo que rompe el test.
#[test]
fn la_superficie_de_comandos_es_la_declarada() {
    let src = std::fs::read_to_string(raiz().join("src/main.rs")).expect("main.rs");
    // El bloque de PRODUCCIÓN, que es el de `not(feature = "metrics")`: la
    // instrumentación de la 3.6 añade un comando más y no puede colarse aquí.
    let (_, tras_cfg) = src
        .split_once("#[cfg(not(feature = \"metrics\"))]")
        .expect("el handler de producción está marcado");
    let (_, resto) = tras_cfg
        .split_once("generate_handler![")
        .expect("el binario registra comandos");
    let (bloque, _) = resto.split_once(']').expect("el macro cierra");
    let registrados: Vec<String> = bloque
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(
        registrados,
        norte_gui_tauri::commands::COMANDOS,
        "la lista declarada y la registrada tienen que ser la misma"
    );
}

/// El bundle de producción no trae nada remoto, ni `eval`, ni el cliente del
/// servidor de desarrollo.
///
/// Si no hay bundle todavía, el test lo DICE y no pasa por alto: un
/// «no había nada que mirar» que se lee como verde es peor que un rojo.
#[test]
fn el_bundle_no_llama_a_casa() {
    let dist = raiz().join("ui/dist");
    let index = dist.join("index.html");
    assert!(
        index.exists(),
        "no hay bundle en {}: córrelo con `just gui-build` antes. Un «no había \
         nada que mirar» que se lee como verde es peor que un rojo — y esto lo \
         decía su propio comentario mientras hacía lo contrario.",
        dist.display()
    );
    let mut mirados = 0usize;
    for entrada in walk(&dist) {
        let Some(ext) = entrada.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(ext, "html" | "js" | "css") {
            continue;
        }
        let texto = std::fs::read_to_string(&entrada).unwrap_or_default();
        mirados += 1;
        for prohibido in [
            "http://",
            "https://",
            "ws://",
            "wss://",
            "eval(",
            "new Function(",
        ] {
            assert!(
                !texto.contains(prohibido),
                "{} contiene `{prohibido}`",
                entrada.display()
            );
        }
    }
    assert!(mirados >= 2, "se miraron el HTML y su script");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// La webview no navega fuera de sus propios assets.
///
/// Dos de los puntos de aceptación de la tarea 3.3 («navegar a
/// `https://example.invalid` se rechaza», «`window.open` no crea una webview
/// sin restricciones») no tenían implementación NI prueba: la CSP no cubre la
/// navegación de primer nivel. Esto clava la guardia que sí lo hace.
#[test]
fn la_webview_no_navega_a_ninguna_parte() {
    let src = std::fs::read_to_string(raiz().join("src/main.rs")).expect("main.rs");
    assert!(
        src.contains("guardia_de_navegacion"),
        "el binario tiene que instalar la guardia de navegación"
    );
    assert!(
        src.contains(r#"const ESQUEMAS_DE_PAGINA: &[&str] = &["tauri", "ipc"];"#),
        "y la lista de esquemas es EXACTAMENTE esa: cualquier añadido es una \
         decisión que se ve en el diff"
    );
}

/// La instrumentación de la 3.6 no viaja en el binario por defecto.
///
/// El test que clava la lista de comandos lee el `main.rs`, así que pasaría
/// igual con `default = ["metrics"]` en el manifiesto: el quinto comando
/// entraría por la puerta de las features y ninguna prueba lo vería.
#[test]
fn la_feature_de_medida_no_es_la_de_por_defecto() {
    let toml = std::fs::read_to_string(raiz().join("Cargo.toml")).expect("Cargo.toml");
    let features = toml
        .split_once("[features]")
        .map(|(_, resto)| resto.split("\n[").next().unwrap_or_default().to_owned())
        .unwrap_or_default();
    assert!(
        features.contains("metrics"),
        "la feature existe y se declara aquí"
    );
    assert!(
        !features.contains("default"),
        "y NO hay `default`: la medida se pide a mano o no está"
    );
}

/// El renderer no llama a un comando que el binario no expone.
///
/// Se mira la FUENTE y no el bundle: el bundler minifica la llamada
/// (`t(`dispatch`)`), así que en `dist` el nombre ya no está pegado a
/// `invoke(` y cualquier barrido allí es adivinar. En `ui/src` sí está, y es
/// donde alguien añadiría un comando nuevo.
#[test]
fn el_renderer_solo_invoca_comandos_conocidos() {
    let src = raiz().join("ui/src");
    let conocidos: Vec<&str> = norte_gui_tauri::commands::COMANDOS
        .iter()
        .copied()
        // `metrics` solo existe con su feature; el renderer lo llama sin
        // condición y el binario de producción lo rechaza.
        .chain(std::iter::once("metrics"))
        .collect();
    let mut vistos = Vec::new();
    for entrada in walk(&src) {
        if entrada.extension().and_then(|e| e.to_str()) != Some("ts") {
            continue;
        }
        let texto = std::fs::read_to_string(&entrada).unwrap_or_default();
        for trozo in texto.split("invoke").skip(1) {
            // `invoke<T>("nombre"` o `invoke("nombre"`.
            let Some(abre) = trozo.find('(') else {
                continue;
            };
            let resto = &trozo[abre + 1..];
            let Some(nombre) = resto
                .trim_start()
                .strip_prefix('"')
                .and_then(|r| r.split('"').next())
            else {
                continue;
            };
            vistos.push(nombre.to_owned());
        }
    }
    assert!(!vistos.is_empty(), "el renderer invoca algo");
    for n in &vistos {
        assert!(
            conocidos.contains(&n.as_str()),
            "el renderer invoca `{n}`, que no está en la superficie declarada"
        );
    }
    // Y los cuatro de producción se usan: una superficie declarada que nadie
    // llama es una superficie que nadie mantiene.
    for c in norte_gui_tauri::commands::COMANDOS {
        assert!(
            vistos.iter().any(|v| v == c),
            "nadie invoca `{c}`: ¿sobra en la lista?"
        );
    }
}

/// La ventana YA muta, y sigue siendo una barrera que se clava aquí.
///
/// El interruptor lo levantó la tarea 5.4 (la revisión de seguridad de las
/// mutaciones que exige el gate de salida de la fase 5), y este test cambió a
/// la vez: mientras valía `SoloLectura`, toda la promesa descansaba en una
/// constante que ninguna prueba miraba, y cambiarla por descuido dejaba la
/// suite entera verde y la ventana borrando ficheros.
///
/// Sigue aquí en el otro sentido: volver a `SoloLectura` también tiene que
/// ser una decisión, no un merge. El rustdoc de la constante dice qué la
/// sostiene.
#[test]
fn la_ventana_muta_y_es_una_decision() {
    assert_eq!(
        norte_gui_tauri::startup::EFECTOS,
        norte_ui_host::commands::Efectos::Completo,
        "cambiar el interruptor de efectos es una decisión de la 5.4, no un descuido"
    );
}
