//! Un provider plugin INSTALADO sirve el scheme que declara.
//!
//! Hasta aquí `[[contributions.provider]]` se declaraba, se aprobaba y se
//! activaba, y nada lo resolvía: el `ConnectionManager` casaba schemes a mano
//! contra los providers del core y un guest FTP embebido. Este test recorre
//! el camino de distribución entero con el guest `provider-mem` compilado de
//! verdad: instalar → consentir → conectar por scheme → listar. Y el
//! negativo: sin consentimiento, el scheme no existe.

use std::path::{Path, PathBuf};
use std::process::Command;

use futures::StreamExt;
use norte_core::connect::{ConnectionManager, RemoteConnector};
use norte_core::plugins::{PluginRegistry, install};
use norte_proto::{Error, Scheme, VPath};

const MANIFEST: &str = r#"
[plugin]
id = "org.norte.memplug"
name = "Mem plug"
publisher = "norte"
version = "0.1.0"
category = "provider"

[[contributions.provider]]
scheme = "memplug"
"#;

/// Un origen instalable con el `.wasm` REAL de `provider-mem`.
fn origen(dir: &Path, wasm: &Path) -> PathBuf {
    let src = dir.join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), MANIFEST).expect("manifest");
    std::fs::copy(wasm, src.join("plugin.wasm")).expect("wasm");
    src
}

#[tokio::test]
async fn un_provider_instalado_y_consentido_sirve_su_scheme() {
    let Some(wasm) = build_guest("provider-mem") else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    let src = origen(cfg.path(), &wasm);
    install(cfg.path(), &src, false).expect("instala");
    let manager = ConnectionManager::new(cfg.path());

    // Instalado pero sin consentir: el scheme no existe para el manager.
    // Fail-closed, y con la misma respuesta que un scheme que nadie sirve.
    let err = manager
        .connect("memplug", "host")
        .await
        .err()
        .expect("sin consentimiento no conecta");
    assert_eq!(err.error, Error::Unsupported);

    {
        let mut reg = PluginRegistry::discover(cfg.path()).expect("discover");
        assert!(
            reg.set_approval("org.norte.memplug", true)
                .expect("aprueba")
        );
        assert!(reg.set_enabled("org.norte.memplug", true).expect("activa"));
    }

    let connected = manager
        .connect("memplug", "host")
        .await
        .map_err(|d| d.error)
        .expect("consentido: conecta");
    assert!(
        connected.warnings.is_empty(),
        "un provider en memoria no avisa de nada"
    );

    // Y es el guest de verdad quien contesta: `provider-mem` siembra un
    // árbol conocido en su raíz.
    let root = VPath::root(Scheme::new("memplug").expect("scheme"), None);
    let names: Vec<Vec<u8>> = connected
        .provider
        .list(&root)
        .await
        .expect("lista la raíz")
        .map(|e| {
            e.expect("entrada")
                .path
                .file_name()
                .expect("con nombre")
                .as_bytes()
                .to_vec()
        })
        .collect()
        .await;
    assert!(!names.is_empty(), "el guest siembra su raíz");
}

/// El mismo guest, declarando `net`: la red que recibe es `ip:puerto` del
/// endpoint de la conexión, resuelto por el host con el filtro anti-SSRF.
const MANIFEST_CON_RED: &str = r#"
[plugin]
id = "org.norte.memplug"
name = "Mem plug"
publisher = "norte"
version = "0.1.0"
category = "provider"

[[contributions.provider]]
scheme = "memplug"

[capabilities]
net = { hosts = [] }
"#;

fn instalar_consentido(cfg: &Path, wasm: &Path, manifest: &str) {
    let src = cfg.join("src-plugin");
    std::fs::create_dir_all(&src).expect("mkdir");
    std::fs::write(src.join("plugin.toml"), manifest).expect("manifest");
    std::fs::copy(wasm, src.join("plugin.wasm")).expect("wasm");
    install(cfg, &src, false).expect("instala");
    let mut reg = PluginRegistry::discover(cfg).expect("discover");
    assert!(
        reg.set_approval("org.norte.memplug", true)
            .expect("aprueba")
    );
    assert!(reg.set_enabled("org.norte.memplug", true).expect("activa"));
}

/// ADR 0093 §3: el host resuelve y filtra. Un endpoint en el rango de
/// metadata no se concede aunque el plugin tenga `net`; loopback tecleado
/// literalmente sí; y sin puerto no hay a qué conceder.
#[tokio::test]
async fn la_red_de_un_provider_plugin_es_el_endpoint_filtrado_y_con_puerto() {
    let Some(wasm) = build_guest("provider-mem") else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    instalar_consentido(cfg.path(), &wasm, MANIFEST_CON_RED);
    let manager = ConnectionManager::new(cfg.path());

    // Metadata de nube: rechazado por el filtro, antes de instanciar nada.
    let err = manager
        .connect("memplug", "169.254.169.254:80")
        .await
        .err()
        .expect("link-local no se concede");
    assert_eq!(err.error, Error::ProviderUnavailable { retryable: true });

    // Sin puerto ni `default-port`: no se sabe a qué conceder.
    let err = manager
        .connect("memplug", "127.0.0.1")
        .await
        .err()
        .expect("sin puerto no hay concesión");
    assert_eq!(err.error, Error::Unsupported);

    // Loopback literal con puerto: se concede `127.0.0.1:1` y el guest (que
    // no abre ninguna conexión) queda configurado.
    manager
        .connect("memplug", "127.0.0.1:1")
        .await
        .map_err(|d| d.error)
        .expect("loopback literal con puerto");
}

/// Lo que corre es lo que se aprobó: cambiar el `.wasm` en disco después de
/// aprobar deja de servir el scheme, aunque el manifiesto no haya cambiado.
#[tokio::test]
async fn un_binario_cambiado_tras_aprobar_no_se_instancia() {
    let Some(wasm) = build_guest("provider-mem") else {
        return;
    };
    let cfg = tempfile::tempdir().expect("tempdir");
    instalar_consentido(cfg.path(), &wasm, MANIFEST);
    let manager = ConnectionManager::new(cfg.path());
    manager
        .connect("memplug", "host")
        .await
        .map_err(|d| d.error)
        .expect("intacto: conecta");

    let instalado = cfg.path().join("plugins/org.norte.memplug/plugin.wasm");
    let mut bytes = std::fs::read(&instalado).expect("lee");
    bytes.push(0);
    std::fs::write(&instalado, bytes).expect("reescribe");
    // El catálogo re-ancla el digest al descubrir, así que la aprobación deja
    // de estar vigente (#241) y el scheme deja de existir.
    let err = manager
        .connect("memplug", "host")
        .await
        .err()
        .expect("binario distinto: no sirve");
    assert_eq!(err.error, Error::Unsupported);
}

/// El scheme del core sigue siendo del core: un plugin no puede reclamarlo, y
/// aunque alguien plantase el directorio a mano el manager no pregunta al
/// catálogo por `sftp`. (La puerta de verdad se prueba en el registro:
/// `resolve_provider_nunca_sirve_un_scheme_del_core`.)
#[tokio::test]
async fn un_scheme_del_core_no_se_consulta_al_catalogo() {
    let cfg = tempfile::tempdir().expect("tempdir");
    let manager = ConnectionManager::new(cfg.path());
    // Sin connections.toml ni servidor, `sftp://` falla por el transporte, no
    // por `Unsupported`: es la prueba de que entró por el brazo del core.
    let err = manager
        .connect("sftp", "127.0.0.1:1")
        .await
        .err()
        .expect("no hay servidor");
    assert_ne!(err.error, Error::Unsupported);
}

fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }
    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("norte-plugin-host")
        .join("examples-wasm")
        .join(name);
    let target_dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("wasm-guests");
    let status = Command::new(env!("CARGO"))
        .current_dir(&guest_dir)
        .args([
            "build",
            "--release",
            "--target",
            "wasm32-wasip2",
            "--target-dir",
        ])
        .arg(&target_dir)
        .status()
        .expect("cargo build del guest");
    assert!(status.success(), "el guest {name} no compiló");
    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(wasm.exists(), "no se encontró {}", wasm.display());
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
