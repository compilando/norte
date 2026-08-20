//! Dónde está el daemon, y de quién es.
//!
//! La dirección del socket la necesitan los DOS extremos: el cliente para
//! conectar y el servidor para atar. Vive aquí, en el crate que ambos pueden
//! ver, porque dos definiciones de la misma ruta de seguridad es exactamente
//! como se divergen (`norte-core` la re-exporta desde `daemon`).

use std::path::PathBuf;

/// Path por defecto del socket: `$XDG_RUNTIME_DIR/norte/daemon.sock`
/// (el runtime dir ya es 0700 por usuario); sin `XDG_RUNTIME_DIR`,
/// `/tmp/norte-<uid>/daemon.sock` — el dir lo crea y VERIFICA el server:
/// dueño = uid del proceso, modo 0700, jamás symlink.
///
/// `uid_hint` solo se usa para el fallback de /tmp (el server lo deriva de
/// su propio socket; los clientes, del dir que encuentran).
#[must_use]
pub fn default_socket_path(uid_hint: Option<u32>) -> PathBuf {
    socket_path_from(
        std::env::var_os("XDG_RUNTIME_DIR"),
        uid_hint.unwrap_or_else(process_uid_best_effort),
    )
}

/// La lógica pura de [`default_socket_path`] (testeable sin tocar el
/// entorno global — que en edición 2024 exige `unsafe`, prohibido aquí).
fn socket_path_from(xdg: Option<std::ffi::OsString>, uid: u32) -> PathBuf {
    if let Some(runtime) = xdg.filter(|v| !v.is_empty()) {
        return PathBuf::from(runtime).join("norte").join("daemon.sock");
    }
    PathBuf::from(format!("/tmp/norte-{uid}")).join("daemon.sock")
}

/// uid del proceso SIN unsafe (regla 5): el dueño de un archivo temporal
/// recién creado por nosotros ES nuestro euid. Solo se usa para NOMBRAR el
/// dir de /tmp y para comparar con el peer del socket; la seguridad real la
/// dan las verificaciones del server sobre dueño y modo.
#[must_use]
pub fn process_uid_best_effort() -> u32 {
    use std::os::unix::fs::MetadataExt;
    let probe = std::env::temp_dir().join(format!(
        ".norte-uid-probe-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos())
    ));
    let uid = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
        .and_then(|f| f.metadata())
        .map(|m| m.uid());
    let _ = std::fs::remove_file(&probe);
    // Fallback imposible en la práctica (temp_dir no escribible): 0 hará
    // que el chequeo anti-root de bind() rechace, fail-safe.
    uid.unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{process_uid_best_effort, socket_path_from};

    #[test]
    fn socket_path_usa_xdg_si_esta() {
        let p = socket_path_from(Some("/run/user/4242".into()), 1000);
        assert_eq!(p, std::path::Path::new("/run/user/4242/norte/daemon.sock"));
    }

    #[test]
    fn socket_path_cae_a_tmp_sin_xdg() {
        assert_eq!(
            socket_path_from(None, 1000),
            std::path::Path::new("/tmp/norte-1000/daemon.sock")
        );
        // XDG vacío = como ausente.
        assert_eq!(
            socket_path_from(Some(String::new().into()), 7),
            std::path::Path::new("/tmp/norte-7/daemon.sock")
        );
    }

    #[test]
    fn uid_best_effort_es_nuestro_euid() {
        use std::os::unix::fs::MetadataExt;
        // El dueño de un archivo que acabamos de crear ES nuestro euid.
        let probe = std::env::temp_dir().join(format!(".norte-uid-test-{}", std::process::id()));
        let f = std::fs::File::create(&probe).expect("crear sonda");
        let expected = f.metadata().expect("metadata").uid();
        drop(f);
        let _ = std::fs::remove_file(&probe);
        assert_eq!(process_uid_best_effort(), expected);
    }
}
