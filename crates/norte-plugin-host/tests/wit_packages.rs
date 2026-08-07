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
    for fichero in ["norte-plugin.wit", "deps/provider/provider.wit"] {
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
