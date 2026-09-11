//! El WIT son TRES paquetes (ADR 0041 decisión 4), y esto vigila que sigan
//! siéndolo.
//!
//! Lo que compra la partición no se ve en ningún test de comportamiento: los
//! guests de `examples-wasm/` se recompilan siempre contra el WIT actual, así
//! que la suite entera pasa igual de verde con un paquete que con tres. Lo que
//! se rompe al volver a juntarlos le pasa a un artefacto `.wasm` que YA está
//! compilado, fuera de este repo, en la máquina de otra persona: la versión del
//! paquete viaja dentro del nombre de cada interfaz, así que un bump de
//! `provider` renombraría `norte:plugin/previewer` y ese previewer dejaría de
//! instanciar.
//!
//! Y `provider` se va a mover: ADR 0041 decisión 3 dice que sus huecos —copia
//! en servidor, papelera, atributos, reanudación, cancelación— se tapan cuando
//! un plugin real los pida. Cada uno de esos es un bump.
//!
//! Por eso esto mira la ESTRUCTURA y no el comportamiento. Es el único sitio
//! donde el daño se puede detectar antes de causarlo.

use std::path::{Path, PathBuf};

use norte_plugin_host::{SERVED_WIT, wit_mismatch, wit_packages};

mod support;

fn wit_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("wit")
}

fn leer(rel: &str) -> String {
    let p = wit_dir().join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("leyendo {}: {e}", p.display()))
}

/// Cada paquete declara su nombre, y son tres distintos.
#[test]
fn son_tres_paquetes_con_nombres_distintos() {
    for (fichero, paquete) in [
        ("norte-plugin.wit", "package norte:plugin@"),
        ("deps/host/host.wit", "package norte:host@"),
        ("deps/provider/provider.wit", "package norte:provider@"),
    ] {
        let src = leer(fichero);
        assert!(
            src.contains(paquete),
            "{fichero} debe declarar `{paquete}…`; si se fusionó con otro paquete, \
             un bump de uno vuelve a invalidar los .wasm del otro (ADR 0041 d4)"
        );
    }
}

/// `provider` NO puede volver al paquete compartido. Es la regresión concreta.
#[test]
fn provider_no_vuelve_al_paquete_compartido() {
    let compartido = leer("norte-plugin.wit");
    assert!(
        !compartido.contains("interface provider"),
        "`interface provider` reapareció en norte:plugin. Cada cambio suyo \
         renombraría norte:plugin/previewer y tumbaría los previewers ya \
         compilados de terceros (ADR 0041 d4)"
    );
    assert!(
        !compartido.contains("world norte-provider"),
        "el world `norte-provider` reapareció en norte:plugin"
    );
}

/// Las dos puertas del host viven en su propio paquete y NO en el compartido:
/// las importan los cuatro worlds, así que compartir paquete con cualquiera de
/// las categorías ata su versión a la de esa categoría.
#[test]
fn las_puertas_del_host_estan_en_su_paquete() {
    let host = leer("deps/host/host.wit");
    for iface in ["interface host-log", "interface host-config"] {
        assert!(host.contains(iface), "`{iface}` debe vivir en norte:host");
    }
    let compartido = leer("norte-plugin.wit");
    for iface in ["interface host-log", "interface host-config"] {
        assert!(
            !compartido.contains(iface),
            "`{iface}` reapareció en norte:plugin"
        );
    }
}

/// Toda referencia cruzada entre paquetes va VERSIONADA. Sin la versión, el
/// resolver no encuentra el paquete —el error real que dio la partición— y el
/// fallo aparece como un `bindgen!` roto, lejos del WIT que lo causó.
#[test]
fn las_referencias_cruzadas_llevan_version() {
    for fichero in [
        "norte-plugin.wit",
        "deps/provider/provider.wit",
        "deps/renamer/renamer.wit",
        "deps/hook/hook.wit",
        "deps/thumbnail/thumbnail.wit",
    ] {
        let src = leer(fichero);
        for (i, linea) in src.lines().enumerate() {
            let l = linea.trim();
            if !l.starts_with("import norte:") && !l.starts_with("use norte:") {
                continue;
            }
            assert!(
                l.contains('@'),
                "{fichero}:{} referencia otro paquete sin versión: `{l}`",
                i + 1
            );
        }
    }
}

/// Lo que el host DICE servir es lo que los ficheros `.wit` declaran. Si
/// alguien sube `norte:plugin` a 0.9.0 y no toca la tabla, el catálogo
/// listaría como rotos los guests recién compilados — o peor, cargaría los
/// viejos sin avisar.
#[test]
fn served_wit_matches_the_package_files() {
    let mut declared: Vec<(String, String)> = Vec::new();
    for fichero in [
        "norte-plugin.wit",
        "deps/host/host.wit",
        "deps/provider/provider.wit",
        "deps/location/location.wit",
        "deps/renamer/renamer.wit",
        "deps/hook/hook.wit",
        "deps/thumbnail/thumbnail.wit",
    ] {
        let src = leer(fichero);
        let linea = src
            .lines()
            .map(str::trim)
            .find(|l| l.starts_with("package norte:"))
            .unwrap_or_else(|| panic!("{fichero} no declara `package norte:…`"));
        let cuerpo = linea.trim_start_matches("package ").trim_end_matches(';');
        let (paquete, version) = cuerpo.split_once('@').expect("versión");
        declared.push((paquete.to_owned(), version.to_owned()));
    }
    declared.sort();
    let mut served: Vec<(String, String)> = SERVED_WIT
        .iter()
        .map(|(p, v)| ((*p).to_owned(), (*v).to_owned()))
        .collect();
    served.sort();
    assert_eq!(
        served, declared,
        "SERVED_WIT no es lo que los .wit declaran"
    );
}

/// Los imports de un guest REAL nombran los paquetes servidos, y un guest
/// recién compilado no es un mismatch.
#[test]
fn wit_imports_of_a_real_guest_name_the_served_versions() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let bytes = std::fs::read(wasm).expect("lee el guest");
    let imports = wit_packages(&bytes);
    assert!(
        imports.contains(&("norte:plugin".to_owned(), "0.10.0".to_owned())),
        "{imports:?}"
    );
    assert!(
        imports.contains(&("norte:host".to_owned(), "0.1.0".to_owned())),
        "{imports:?}"
    );
    assert!(wit_mismatch(&imports).is_none());
}

/// Un guest compilado contra otra versión del paquete es un mismatch con las
/// DOS versiones en la mano: la suya y la servida. Se fabrica reescribiendo
/// `@0.10.0` por `@0.70.0` en los bytes del guest real — misma longitud, así
/// que las secciones siguen siendo válidas y el lector las recorre.
#[test]
fn a_guest_built_against_another_version_is_a_mismatch() {
    let Some(wasm) = support::build_guest("previewer-demo") else {
        return;
    };
    let bytes = std::fs::read(wasm).expect("lee el guest");
    // El lado de los EXPORTS (`norte:plugin`). `0.70.0`: una versión que el
    // host no sirve para ningún paquete, para que confundir paquete y
    // versión no pase por casualidad.
    let viejo = support::rewrite_bytes(&bytes, b"@0.10.0", b"@0.70.0");
    let imports = wit_packages(&viejo);
    let m = wit_mismatch(&imports).expect("mismatch");
    assert_eq!(m.package, "norte:plugin");
    assert_eq!(m.built_against, "0.70.0");
    assert_eq!(m.served, "0.10.0");

    // Y el lado de los IMPORTS (`norte:host`): cualquiera de los dos puede
    // estar desfasado.
    let viejo = support::rewrite_bytes(&bytes, b"@0.1.0", b"@0.0.9");
    let imports = wit_packages(&viejo);
    let m = wit_mismatch(&imports).expect("mismatch en imports");
    assert_eq!(m.package, "norte:host");
    assert_eq!(m.built_against, "0.0.9");
    assert_eq!(m.served, "0.1.0");
}

/// Bytes que no son un componente no tienen imports: ni error ni pánico, que
/// es lo que el catálogo necesita para que un `plugin.wasm` basura siga
/// siendo «sin binario» y no «catálogo caído».
#[test]
fn bytes_that_are_not_a_component_have_no_imports() {
    assert!(wit_packages(b"\0asm\x01\0\0\0").is_empty());
    assert!(wit_packages(b"garbage").is_empty());
    assert!(wit_packages(b"").is_empty());
    assert!(wit_mismatch(&[]).is_none());
}

/// El paquete compartido importa del de host, nunca al revés: `norte:host` es
/// la hoja del grafo. Un ciclo aquí no lo detecta nadie hasta que el resolver
/// se queja, y su mensaje no dice cuál de los dos lados sobra.
#[test]
fn host_no_depende_de_nadie() {
    let host = leer("deps/host/host.wit");
    for linea in host.lines() {
        let l = linea.trim();
        assert!(
            !(l.starts_with("import norte:") || l.starts_with("use norte:")),
            "norte:host debe ser la hoja del grafo, y depende de algo: `{l}`"
        );
    }
}
