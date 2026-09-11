//! Contra qué WIT se compiló un guest, leído del binario (ADR 0094).
//!
//! La versión de un paquete WIT viaja DENTRO del nombre de cada interfaz que
//! un componente importa o exporta (`norte:host/host-log@0.1.0`,
//! `norte:plugin/previewer@0.8.0`), así que un bump —cualquiera— hace que un
//! `.wasm` ya compilado no instancie: wasmtime falla nombrando la interfaz
//! que falta, y nada más. Este módulo lee esos nombres sin compilar nada,
//! para que el catálogo pueda decir «compilado contra `norte:plugin@0.7.0`,
//! este norte sirve `@0.8.0`» y listar el plugin como roto con ese motivo.
//!
//! Se miran imports Y exports: un previewer IMPORTA `norte:host` y EXPORTA
//! `norte:plugin`, y las dos versiones tienen que casar.
//!
//! El host sirve UNA versión de cada paquete ([`SERVED_WIT`]), sin ventana de
//! compatibilidad: mantenerla querría decir dejar linkado cada world viejo
//! para siempre, y el primer plugin que pida un hueco del WIT es el
//! argumento para no prometerlo todavía.

use wasmparser::{Parser, Payload};

/// La única versión de cada paquete `norte:*` que este host sirve.
///
/// Un test estructural (`tests/wit_packages.rs`) la compara con las líneas
/// `package …;` de los ficheros `.wit`: subir un paquete sin tocar esto
/// listaría como rotos los guests recién compilados.
pub const SERVED_WIT: &[(&str, &str)] = &[
    ("norte:host", "0.1.0"),
    ("norte:plugin", "0.10.0"),
    ("norte:provider", "0.1.0"),
    ("norte:location", "0.2.0"),
    ("norte:renamer", "0.1.0"),
    ("norte:hook", "0.2.0"),
    ("norte:thumbnail", "0.1.0"),
];

/// Un guest compilado contra una versión de un paquete que el host sirve a
/// OTRA versión.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WitMismatch {
    /// El paquete (`norte:plugin`).
    pub package: String,
    /// La versión que el guest referencia.
    pub built_against: String,
    /// La que este host sirve.
    pub served: String,
}

/// Los pares `(paquete, versión)` de los paquetes `norte:*` que un componente
/// importa o exporta, ordenados y sin repetir. Un módulo core (no un
/// componente), unos bytes que no parsean, o un componente que no nombra
/// ningún paquete de norte: vector vacío — nunca un error ni un pánico,
/// porque el catálogo lo llama sobre lo que haya en `plugin.wasm`.
///
/// Recorre también los componentes ANIDADOS: un guest que embeba un
/// componente que nombre `norte:plugin@0.7.0` se lista como desfasado
/// aunque wasmtime solo enlace los nombres del exterior. Es un falso
/// positivo posible, nunca un falso pase, y ningún guest de norte anida
/// componentes hoy. Quien lo necesite estrecha esto a la sección exterior.
///
/// El coste es lineal en el tamaño del fichero, que el catálogo acota ANTES
/// de leerlo ([`crate::MAX_ARTIFACT_BYTES`]); `wasmparser` no descomprime ni
/// recurre.
///
/// ```
/// use norte_plugin_host::wit_packages;
/// assert!(wit_packages(b"garbage").is_empty());
/// assert!(wit_packages(b"\0asm\x01\0\0\0").is_empty());
/// ```
#[must_use]
pub fn wit_packages(bytes: &[u8]) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for payload in Parser::new(0).parse_all(bytes) {
        let Ok(payload) = payload else {
            // Bytes rotos a partir de aquí: lo recogido hasta ahora vale.
            break;
        };
        match payload {
            Payload::ComponentImportSection(section) => {
                for import in section {
                    let Ok(import) = import else { break };
                    out.extend(parse_norte_name(import.name.name));
                }
            }
            Payload::ComponentExportSection(section) => {
                for export in section {
                    let Ok(export) = export else { break };
                    out.extend(parse_norte_name(export.name.name));
                }
            }
            _ => {}
        }
    }
    out.sort();
    out.dedup();
    out
}

/// `norte:<pkg>/<iface>@<ver>` → `(norte:<pkg>, <ver>)`; cualquier otra forma,
/// `None`.
///
/// La versión viene del BINARIO, y el binario lo escribe un tercero: solo se
/// acepta una con forma de versión (`[A-Za-z0-9.+-]`, 64 bytes como mucho).
/// Lo que no la tenga no es un nombre de norte y no produce mismatch — y la
/// cadena que acaba en el gestor, en `plugin list` y en `norte doctor` no
/// puede llevar un escape de terminal ni cien kilobytes. `wasmparser` solo
/// garantiza UTF-8.
fn parse_norte_name(name: &str) -> Option<(String, String)> {
    let rest = name.strip_prefix("norte:")?;
    let (pkg, tail) = rest.split_once('/')?;
    let (_iface, version) = tail.rsplit_once('@')?;
    let version_ok = !version.is_empty()
        && version.len() <= 64
        && version
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b".+-".contains(&b));
    let pkg_ok = !pkg.is_empty()
        && pkg.len() <= 64
        && pkg
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
    if !version_ok || !pkg_ok {
        return None;
    }
    Some((format!("norte:{pkg}"), version.to_owned()))
}

/// El primer paquete que el host sirve a OTRA versión, o `None` si todo casa
/// (o si el guest no nombra nada de norte).
///
/// ```
/// use norte_plugin_host::wit_mismatch;
/// let ok = vec![("norte:host".to_owned(), "0.1.0".to_owned())];
/// assert!(wit_mismatch(&ok).is_none());
/// let viejo = vec![("norte:plugin".to_owned(), "0.1.0".to_owned())];
/// let m = wit_mismatch(&viejo).unwrap();
/// assert_eq!((m.package.as_str(), m.built_against.as_str()), ("norte:plugin", "0.1.0"));
/// ```
#[must_use]
pub fn wit_mismatch(packages: &[(String, String)]) -> Option<WitMismatch> {
    packages.iter().find_map(|(package, version)| {
        let (_, served) = SERVED_WIT.iter().find(|(p, _)| *p == package)?;
        (served != version).then(|| WitMismatch {
            package: package.clone(),
            built_against: version.clone(),
            served: (*served).to_owned(),
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parsea_el_nombre_de_una_interfaz_de_norte() {
        assert_eq!(
            parse_norte_name("norte:host/host-log@0.1.0"),
            Some(("norte:host".to_owned(), "0.1.0".to_owned()))
        );
        assert_eq!(parse_norte_name("wasi:io/streams@0.2.0"), None);
        assert_eq!(parse_norte_name("norte:host/host-log"), None);
        assert_eq!(parse_norte_name("norte:/x@1"), None);
    }

    /// La versión la escribe el binario de un tercero: un escape de terminal
    /// o cien kilobytes tras la `@` no es una versión, y no llega a ninguna
    /// pantalla.
    #[test]
    fn una_version_que_no_tiene_forma_de_version_no_es_un_nombre_de_norte() {
        assert_eq!(
            parse_norte_name("norte:plugin/previewer@\u{1b}]0;x\u{7}"),
            None
        );
        let larga = format!("norte:plugin/previewer@{}", "9".repeat(100_000));
        assert_eq!(parse_norte_name(&larga), None);
        assert_eq!(
            parse_norte_name("norte:plugin/previewer@0.9.0-rc.1+b"),
            Some(("norte:plugin".to_owned(), "0.9.0-rc.1+b".to_owned()))
        );
        assert_eq!(parse_norte_name("norte:Plu gin/x@1.0.0"), None);
    }
}
