//! El catálogo del protocolo NO se queda atrás (ADR 0089).
//!
//! Un método nuevo se toca en muchos sitios. Ninguno sobra, y el reparto plano
//! del daemon es deliberado. Lo que faltaba era que **olvidar uno se notara**.
//!
//! Este fichero cierra la primera puerta: una constante de método que no esté
//! en el catálogo pone el gate en rojo. Las demás superficies —el reparto del
//! daemon, el cliente remoto— las comprueba `norte-core/tests/catalogo_rpc.rs`,
//! que es donde se las puede leer.
//!
//! Se lee el CÓDIGO y no una lista escrita a mano, por el mismo motivo por el
//! que lo hace el barrido de claves del host: una lista se separa del código en
//! la primera superficie nueva.

use norte_proto::catalog::{CATALOGO, Kind};

/// Constantes que NO son métodos de wire, con el motivo por el que se saltan.
const NO_SON_METODOS: &[&str] = &[
    // La versión del protocolo, que es un número y no una llamada.
    "PROTOCOL_VERSION",
];

/// Las constantes de método declaradas en `methods.rs`: `NOMBRE` → `wire`.
fn constantes_declaradas() -> Vec<(String, String)> {
    let ruta = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/methods.rs");
    let texto =
        std::fs::read_to_string(&ruta).unwrap_or_else(|e| panic!("se lee {}: {e}", ruta.display()));
    let mut out = Vec::new();
    for linea in texto.lines() {
        let l = linea.trim();
        let Some(resto) = l.strip_prefix("pub const ") else {
            continue;
        };
        let Some((nombre, valor)) = resto.split_once(": &str = ") else {
            continue;
        };
        let valor = valor.trim().trim_end_matches(';').trim_matches('"');
        out.push((nombre.to_owned(), valor.to_owned()));
    }
    assert!(
        out.len() > 60,
        "el barrido tiene que ver el protocolo entero, y ve {}",
        out.len()
    );
    out
}

/// **Toda constante de método está en el catálogo.**
///
/// Es la que convierte el catálogo en algo que no se puede olvidar: añadir
/// `pub const FS_LOQUESEA` y no registrarlo deja este test en rojo, con el
/// nombre delante.
#[test]
fn ninguna_constante_se_queda_fuera_del_catalogo() {
    let catalogados: std::collections::BTreeSet<&str> = CATALOGO.iter().map(|m| m.name).collect();
    let mut faltan = Vec::new();
    for (nombre, wire) in constantes_declaradas() {
        if NO_SON_METODOS.contains(&nombre.as_str()) {
            continue;
        }
        if !catalogados.contains(wire.as_str()) {
            faltan.push(format!("{nombre} (\"{wire}\")"));
        }
    }
    assert!(
        faltan.is_empty(),
        "métodos sin registrar en `catalog.rs`: {}.\n\
         Un método que no está en el catálogo no lo comprueba nadie: ni que el \
         daemon lo reparta, ni que el cliente lo sepa pedir, ni que su tipo \
         esté en el schema.",
        faltan.join(", ")
    );
}

/// Y al revés: el catálogo no nombra métodos que no existen.
///
/// Sin esto, borrar una constante dejaría una entrada fantasma que los demás
/// tests darían por buena — y el gate seguiría verde sobre un método que ya no
/// está. (El compilador caza el nombre de la CONSTANTE; esto caza el de wire.)
#[test]
fn el_catalogo_no_nombra_lo_que_no_existe() {
    let declarados: std::collections::BTreeSet<String> = constantes_declaradas()
        .into_iter()
        .map(|(_, wire)| wire)
        .collect();
    for m in CATALOGO {
        assert!(
            declarados.contains(m.name),
            "el catálogo nombra `{}`, que ya no se declara en methods.rs",
            m.name
        );
    }
}

/// **El catálogo tiene GOLDEN, así que borrar o renombrar un método se ve.**
///
/// Hoy nada protegía los nombres de wire. `docs/schema/proto.schema.json`
/// publica TIPOS, no métodos, y el único golden con nombres de método
/// (`tests/golden/types/envelope.json`) contiene cuatro. Borrar
/// `fs.rename_batch` —una rotura de wire de manual— no ponía nada en rojo más
/// allá de la compilación de sus llamantes.
///
/// El catálogo es ahora la única lista completa, y sin instantánea tampoco
/// protegería: el test de arriba solo comprueba que no nombre lo que ya no
/// existe, o sea que ACOMPAÑA al borrado en vez de resistirse a él. Esto lo
/// resiste: cualquier alta, baja o cambio de forma sale como un diff que el
/// revisor ve.
///
/// Se regenera con `NORTE_UPDATE_GOLDEN=1`, como los demás.
#[test]
fn el_catalogo_tiene_golden() {
    let mut lineas: Vec<String> = CATALOGO
        .iter()
        .map(|m| {
            format!(
                "{}\t{:?}\t{:?}\t{}\t{}",
                m.name, m.kind, m.shape, m.params_ty, m.result_ty
            )
        })
        .collect();
    lineas.sort();
    let actual = format!("{}\n", lineas.join("\n"));

    let ruta = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/catalogo.tsv");
    if std::env::var_os("NORTE_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(ruta.parent().expect("tiene padre")).expect("se crea el dir");
        std::fs::write(&ruta, &actual).expect("se escribe el golden");
        return;
    }
    let esperado = std::fs::read_to_string(&ruta).unwrap_or_else(|e| {
        panic!(
            "no está el golden {} ({e}). Genéralo con NORTE_UPDATE_GOLDEN=1",
            ruta.display()
        )
    });
    assert_eq!(
        actual, esperado,
        "el catálogo del protocolo cambió. Si es a propósito, regenera con \
         NORTE_UPDATE_GOLDEN=1 y que el diff se REVISE: un método que \
         desaparece o cambia de forma es una rotura de wire."
    );
}

/// El catálogo cubre el protocolo entero, no una muestra.
#[test]
fn el_catalogo_no_esta_a_medias() {
    let peticiones = CATALOGO.iter().filter(|m| m.kind == Kind::Request).count();
    let notificaciones = CATALOGO
        .iter()
        .filter(|m| m.kind == Kind::Notification)
        .count();
    assert!(peticiones > 55, "peticiones catalogadas: {peticiones}");
    assert!(
        notificaciones >= 8,
        "notificaciones catalogadas: {notificaciones}"
    );
}
