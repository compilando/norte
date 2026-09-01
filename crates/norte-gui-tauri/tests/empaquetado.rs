//! Lo que el PAQUETE promete, clavado (#256).
//!
//! Estas comprobaciones leen la configuración del bundler y la entrada de
//! escritorio, no el runtime, y ese es el punto: un paquete roto no se ve en
//! ninguna prueba de comportamiento. Se ve al instalarlo en una máquina limpia
//! —que es cuando ya es tarde— o se ve aquí.
//!
//! El empaquetado se ejercitó por primera vez el 2026-08-25 y salieron cuatro
//! cosas de golpe: la descripción decía «Spike vertical del renderer de
//! Tauri», la categoría era `Utility` a secas, no había `MimeType` (o sea que
//! «abrir carpeta con…» no ofrecía norte) y el `Exec` entregaba una URL a un
//! binario que espera una ruta. Todas son de esta clase: invisibles hasta que
//! alguien instala.

use std::path::PathBuf;

fn raiz() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn conf() -> serde_json::Value {
    let p = raiz().join("tauri.conf.json");
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("no se pudo leer {}: {e}", p.display()));
    serde_json::from_str(&raw).expect("JSON válido")
}

fn desktop() -> String {
    let p = raiz().join("norte.desktop");
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("no se pudo leer {}: {e}", p.display()))
}

/// Una clave de la entrada de escritorio, sin comentarios ni espacios.
fn clave(texto: &str, k: &str) -> Option<String> {
    texto
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .find_map(|l| l.strip_prefix(&format!("{k}=")))
        .map(str::to_owned)
}

/// **El paquete trae el DAEMON y el CLI, no solo la ventana.**
///
/// Sin ellos, en una instalación limpia la ventana no tiene a quién pedirle un
/// listado: `norte-gui` busca a `norte` como HERMANO de su propio ejecutable, y
/// si no está no hay daemon que arrancar. Era la pregunta abierta de #256.
#[test]
fn el_paquete_trae_el_daemon_y_el_cli() {
    let cfg = conf();
    let externos: Vec<&str> = cfg["bundle"]["externalBin"]
        .as_array()
        .expect("hay binarios externos")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert!(
        externos.iter().any(|b| b.ends_with("/norte")),
        "el daemon viaja con la ventana: {externos:?}"
    );
    assert!(
        externos.iter().any(|b| b.ends_with("/ntc")),
        "y el cliente de terminal: {externos:?}"
    );
}

/// **La descripción larga describe norte, no el andamio con el que se probó.**
///
/// Es lo que se lee en el gestor de paquetes, y decía «Spike vertical del
/// renderer de Tauri sobre norte-ui-host» — una frase de bitácora interna
/// delante de quien está decidiendo si instalar esto.
/// **El identificador no dice `spike`.**
///
/// Es el id de aplicación: nombra el directorio de datos de la webview, la
/// entrada de escritorio y el paquete. Mientras decía `dev.norte.gui.spike`,
/// cualquiera que mirara qué hay instalado leía que esto es un experimento —
/// y lo era, hasta que el go/no-go se cerró (ADR 0087).
///
/// Cambiarlo TIENE precio y por eso se hizo ahora: una instalación con el id
/// viejo no se actualiza encima, se queda al lado. En alfa el precio es cero;
/// después de la primera versión estable habría sido una migración.
#[test]
fn el_identificador_no_dice_spike() {
    let cfg = conf();
    let id = cfg["identifier"].as_str().expect("hay identifier");
    assert!(
        !id.contains("spike"),
        "el id de aplicación sigue diciendo que esto es un experimento: {id}"
    );
    assert_eq!(id, "dev.norte.gui", "y es el que el paquete promete");
}

#[test]
fn la_descripcion_no_habla_de_un_spike() {
    let cfg = conf();
    let larga = cfg["bundle"]["longDescription"]
        .as_str()
        .expect("hay descripción larga")
        .to_lowercase();
    assert!(
        !larga.contains("spike"),
        "la descripción del paquete no es una nota de desarrollo: {larga:?}"
    );
    assert!(larga.contains("ficheros"), "y dice qué es esto: {larga:?}");
}

/// **`Exec` entrega una RUTA, no una URL.**
///
/// `%U` da `file:///casa`, y el binario lo mete en un `PathBuf`: eso es una
/// ruta RELATIVA que no existe, así que la ventana abriría sobre un error en
/// vez de sobre la carpeta que el escritorio acaba de nombrar. `%f` da la ruta
/// local, que es lo que `norte-gui [DIR]` sabe leer.
#[test]
fn el_exec_entrega_una_ruta_y_no_una_url() {
    let d = desktop();
    let exec = clave(&d, "Exec").expect("hay Exec");
    assert!(
        !exec.contains("%U") && !exec.contains("%u"),
        "una URL no es una ruta: {exec:?}"
    );
    assert!(
        exec.contains("%f") || exec.contains("%F"),
        "y se le pasa la carpeta: {exec:?}"
    );
}

/// **Se ofrece para abrir carpetas.**
///
/// Sin `MimeType=inode/directory`, «abrir con…» sobre una carpeta no lista
/// norte. Para un gestor de ficheros eso es la integración entera con el
/// escritorio, y su ausencia no se nota en nada más.
#[test]
fn el_escritorio_lo_ofrece_para_abrir_carpetas() {
    let d = desktop();
    let mime = clave(&d, "MimeType").unwrap_or_default();
    assert!(
        mime.contains("inode/directory"),
        "un gestor de ficheros abre directorios: {mime:?}"
    );
    let cats = clave(&d, "Categories").unwrap_or_default();
    assert!(
        cats.contains("FileManager"),
        "y se declara como lo que es: {cats:?}"
    );
    // UNA categoría principal. Con dos, la aplicación puede salir DOS VECES en
    // el menú, y eso lo avisa `desktop-file-validate` — que no corre en el
    // gate, así que la comprobación vive aquí.
    let principales = [
        "AudioVideo",
        "Audio",
        "Video",
        "Development",
        "Education",
        "Game",
        "Graphics",
        "Network",
        "Office",
        "Science",
        "Settings",
        "System",
        "Utility",
    ];
    let cuantas = principales
        .iter()
        .filter(|p| cats.split(';').any(|c| c == **p))
        .count();
    assert_eq!(
        cuantas, 1,
        "una sola categoría principal, o sale dos veces en el menú: {cats:?}"
    );
}

/// La entrada de escritorio que se empaqueta es la NUESTRA, no la que el
/// bundler genera.
///
/// Sin esta línea en la configuración, todo lo anterior se escribe en un
/// fichero que nadie usa: el bundler compone el suyo y descarta este.
#[test]
fn la_entrada_de_escritorio_es_la_que_esta_en_el_repo() {
    let cfg = conf();
    assert_eq!(
        cfg["bundle"]["linux"]["deb"]["desktopTemplate"].as_str(),
        Some("norte.desktop"),
        "el bundler tiene que usar la plantilla del repo"
    );
    assert!(
        raiz().join("norte.desktop").is_file(),
        "y esa plantilla existe"
    );
}
