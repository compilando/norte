//! `org.norte.by-extension` (`plugins/by-extension/`) compila, se instala y
//! propone a dónde va cada nombre (fase 8, paquete WIT `norte:organizer`).
//!
//! Es el ejercicio de punta a punta del ABI nuevo, y por eso el plugin es lo
//! más tonto posible —la respuesta está en el nombre, sin capacidades—: si
//! esto falla, falla el ABI y no la astucia del guest.
//!
//! Lo que además fija: que un destino que se sale del directorio NO llega a
//! aplicarse. El host valida la propuesta de un tercero con la MISMA función
//! que valida la de un modelo, y tumba el plan entero en vez de aceptar «lo
//! que se pudo».

use std::path::{Path, PathBuf};
use std::process::Command;

use norte_core::plugins::{OrganizePlanOutcome, PluginRegistry, install, run_organize_plan};
use norte_plugin_host::PluginRuntime;

const ID: &str = "org.norte.by-extension";
const ORGANIZER: &str = "by-extension";

fn plugin_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../plugins/by-extension")
}

fn build_plugin() -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(plugin_dir())
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo for by-extension");
    assert!(status.success(), "by-extension did not build");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join("by_extension.wasm");
    assert!(wasm.exists(), "missing {}", wasm.display());
    Some(wasm)
}

fn target_installed(target: &str) -> bool {
    Command::new("rustup")
        .args(["target", "list", "--installed"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .is_some_and(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l == target)
        })
}

fn install_and_consent(cfg: &Path, wasm: &Path) -> PluginRegistry {
    let stage = cfg.join("stage");
    std::fs::create_dir_all(&stage).expect("stage");
    std::fs::copy(plugin_dir().join("plugin.toml"), stage.join("plugin.toml")).expect("manifest");
    std::fs::copy(wasm, stage.join("plugin.wasm")).expect("wasm");
    assert_eq!(install(cfg, &stage, false).expect("installs").id, ID);
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
    reg
}

/// El camino feliz: un plugin `organizer` de verdad, cargado por el host de
/// verdad, contesta a dónde va cada nombre.
#[test]
fn by_extension_propone_una_carpeta_por_extension() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let reg = install_and_consent(cfg.path(), &wasm);
    let rt = PluginRuntime::new().expect("runtime");

    let names: Vec<String> = ["factura.pdf", "foto.JPG", "LEEME", ".bashrc"]
        .iter()
        .map(|n| (*n).to_owned())
        .collect();

    let resolved = reg
        .resolve_organizer(ID, ORGANIZER)
        .expect("el manifiesto declara el organizer");
    let outcome = run_organize_plan(&rt, resolved, ORGANIZER, None, false, &names);
    let OrganizePlanOutcome::Plan(moves) = outcome else {
        panic!("esperaba un plan, salió {outcome:?}");
    };
    let pares: Vec<(&str, &str)> = moves
        .iter()
        .map(|m| (m.current.as_str(), m.proposed_rel.as_str()))
        .collect();
    assert_eq!(
        pares,
        vec![
            ("factura.pdf", "pdf/factura.pdf"),
            ("foto.JPG", "jpg/foto.JPG"),
            ("LEEME", "sin-extension/LEEME"),
            (".bashrc", "sin-extension/.bashrc"),
        ],
        "la extensión en minúsculas, y un punto inicial no es extensión"
    );
}

/// Un organizer que no se ha aprobado no contesta, y uno que se pide por un
/// id que no declara tampoco: la misma puerta que el renamer.
#[test]
fn un_organizer_sin_aprobar_o_con_otro_id_no_resuelve() {
    let Some(wasm) = build_plugin() else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let stage = cfg.path().join("stage");
    std::fs::create_dir_all(&stage).expect("stage");
    std::fs::copy(plugin_dir().join("plugin.toml"), stage.join("plugin.toml")).expect("manifest");
    std::fs::copy(&wasm, stage.join("plugin.wasm")).expect("wasm");
    install(cfg.path(), &stage, false).expect("installs");
    let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");

    // Recién instalado: ni aprobado ni encendido.
    assert!(
        reg.resolve_organizer(ID, ORGANIZER).is_none(),
        "sin aprobar no se resuelve"
    );

    assert!(reg.set_approval(ID, true).expect("approve"));
    assert!(reg.set_enabled(ID, true).expect("enable"));
    assert!(
        reg.resolve_organizer(ID, "otro-id").is_none(),
        "un id que el manifiesto no declara tampoco"
    );
    assert!(reg.resolve_organizer("org.acme.nope", ORGANIZER).is_none());
}
