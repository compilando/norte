//! Soporte de tests: compila un guest WASM de `examples-wasm/` a
//! `wasm32-wasip2` bajo demanda y devuelve la ruta del componente.
//!
//! Si el target `wasm32-wasip2` no está instalado, el helper hace SKIP
//! (devuelve `None`) para que el test pase en toolchains sin ese target; si el
//! target ESTÁ pero el guest no compila, es un fallo real y aborta.

use std::path::PathBuf;
use std::process::Command;

/// Compila el guest `examples-wasm/<name>/` a `wasm32-wasip2` en modo release y
/// devuelve la ruta del `.wasm` producido.
///
/// Devuelve `None` (con un aviso por `stderr`) si el target `wasm32-wasip2` no
/// está instalado.
#[must_use]
pub fn build_guest(name: &str) -> Option<PathBuf> {
    if !target_installed("wasm32-wasip2") {
        eprintln!("SKIP: target wasm32-wasip2 no instalado");
        return None;
    }

    let guest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
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
        .expect("no se pudo lanzar cargo para compilar el guest");
    assert!(
        status.success(),
        "el guest {name} no compiló (target wasm32-wasip2 presente)"
    );

    let wasm = target_dir
        .join("wasm32-wasip2")
        .join("release")
        .join(format!("{}.wasm", name.replace('-', "_")));
    assert!(
        wasm.exists(),
        "no se encontró el artefacto {}",
        wasm.display()
    );
    Some(wasm)
}

/// Reemplaza CADA aparición de `from` por `to` en `bytes`. Solo con
/// longitudes iguales: es para fabricar un guest «compilado contra otra
/// versión» reescribiendo `@0.8.0` en su sección de imports sin mover ni un
/// offset de las demás secciones.
///
/// # Panics
/// Si las longitudes difieren: un reemplazo que desplaza bytes deja un
/// componente que ningún lector recorre, y el test estaría probando basura.
#[must_use]
#[allow(dead_code)]
pub fn rewrite_bytes(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    assert_eq!(from.len(), to.len(), "solo reemplazos de la misma longitud");
    let mut out = bytes.to_vec();
    if from.is_empty() {
        return out;
    }
    let mut i = 0;
    while i + from.len() <= out.len() {
        if &out[i..i + from.len()] == from {
            out[i..i + from.len()].copy_from_slice(to);
            i += from.len();
        } else {
            i += 1;
        }
    }
    out
}

/// `true` si `rustup` reporta `target` entre los instalados.
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

/// Un [`LocationHost`](norte_plugin_host::LocationHost) de mentira que CUENTA
/// las veces que se le pregunta: así un test puede afirmar que el host no
/// resolvió nada, que es distinto de que resolviera y el guest tirara el dato.
// `support` se compila DENTRO de cada binario de test, y solo `columns_e2e`
// usa el espía: en los demás está muerto por construcción, no por olvido.
#[allow(dead_code)]
#[derive(Debug, Default)]
pub struct SpyLocation {
    calls: std::sync::atomic::AtomicUsize,
}

#[allow(dead_code)]
impl SpyLocation {
    /// Cuántas veces se le ha preguntado algo.
    #[must_use]
    pub fn calls(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn count(&self) {
        self.calls
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl norte_plugin_host::LocationHost for SpyLocation {
    fn read(&self, _token: &str, rel: &[u8]) -> Result<Vec<u8>, String> {
        self.count();
        if rel == b"a.txt" {
            Ok(b"contenido".to_vec())
        } else {
            Err("no existe".into())
        }
    }

    fn read_prefix(&self, token: &str, rel: &[u8], max: u64) -> Result<Vec<u8>, String> {
        let mut bytes = self.read(token, rel)?;
        bytes.truncate(usize::try_from(max).unwrap_or(usize::MAX));
        Ok(bytes)
    }

    fn stat(
        &self,
        _token: &str,
        rel: &[u8],
    ) -> Result<norte_plugin_host::location_iface::Meta, String> {
        self.count();
        if rel != b"a.txt" {
            return Err("no existe".into());
        }
        Ok(norte_plugin_host::location_iface::Meta {
            kind: norte_plugin_host::location_iface::EntryKind::File,
            size: 42,
            mtime_sec: 1,
            mtime_nsec: 0,
            ctime_sec: 1,
            ctime_nsec: 0,
            ino: 7,
            dev: 9,
            mode: 0o100_644,
        })
    }

    fn list_dir(
        &self,
        _token: &str,
        _rel: &[u8],
    ) -> Result<Vec<norte_plugin_host::location_iface::Dirent>, String> {
        self.count();
        Ok(Vec::new())
    }
}

/// Capabilities con `location = "read"` concedida.
#[allow(dead_code)]
#[must_use]
pub fn caps_con_location() -> norte_plugin_host::Capabilities {
    norte_plugin_host::Manifest::from_toml(
        r#"
[plugin]
id = "org.norte.columnas"
name = "Columnas"
publisher = "norte"
version = "0.1.0"
category = "columns"

[capabilities]
location = "read"
"#,
    )
    .expect("manifiesto de prueba válido")
    .capabilities
}
