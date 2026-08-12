//! `provider_contract!` sobre el FS real (tempdir): la misma suite que pasa
//! `MemProvider`, contra disco. En CI corre en los 3 OS (fase 12).

use norte_vfs_local::LocalProvider;

fn hostile() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod local_fs,
    factory: {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = dir.path().to_path_buf();
        // La papelera, DENTRO de la raíz del provider: así el contrato puede
        // exigir que el destino recuperable sea una ruta que este provider
        // resuelve, y de paso ningún test acaba en la papelera de verdad del
        // desarrollador. Se crea sola la primera vez que se entierra algo, así
        // que los demás tests del contrato no ven ninguna entrada de más.
        LocalProvider::rooted(base.clone())
            .with_trash_home(base.join(".xdg"))
            .with_guard(Box::new(dir))
    },
    root: LocalProvider::root(),
    hostile_names: hostile(),
}
