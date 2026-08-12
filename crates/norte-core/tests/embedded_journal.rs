//! #167: el engine embebido lleva journal, y DOS a la vez no.

use norte_core::embedded::EmbeddedJournal;

/// Un directorio de estado limpio da un engine journalizado.
#[tokio::test]
async fn el_primero_se_lleva_el_journal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let abierto = EmbeddedJournal::open_in(dir.path()).await;
    assert!(
        matches!(abierto, EmbeddedJournal::Owned(_)),
        "un directorio de estado libre da journal: {abierto:?}"
    );
}

/// El segundo proceso NO forkea la cadena: el lock exclusivo de `SQLite` lo
/// rechaza, y el engine sale sin journal en vez de sin arrancar.
#[tokio::test]
async fn el_segundo_se_queda_sin_journal_pero_arranca() {
    let dir = tempfile::tempdir().expect("tempdir");
    let primero = EmbeddedJournal::open_in(dir.path()).await;
    assert!(matches!(primero, EmbeddedJournal::Owned(_)));

    let t0 = std::time::Instant::now();
    let segundo = EmbeddedJournal::open_in(dir.path()).await;
    let tardo = t0.elapsed();
    assert!(
        matches!(segundo, EmbeddedJournal::Busy),
        "el segundo no puede escribir la misma cadena: {segundo:?}"
    );
    // Y se entera PRONTO. Con el `busy_timeout` de `sqlx` por omisión esto
    // tardaba 5 s: un `norte cp` con el daemon vivo se quedaba cinco segundos
    // parado para acabar haciendo exactamente lo mismo, sin registro. El plazo
    // corto de `embedded` es lo que se pinea aquí; el margen es generoso porque
    // esto corre en una máquina de CI cargada.
    assert!(
        tardo < std::time::Duration::from_secs(2),
        "rendirse con el lock ajeno tiene que ser rápido, tardó {tardo:?}"
    );
}

/// Soltar al primero libera el lock: el siguiente vuelve a llevárselo.
#[tokio::test]
async fn soltarlo_devuelve_el_journal_al_siguiente() {
    let dir = tempfile::tempdir().expect("tempdir");
    drop(EmbeddedJournal::open_in(dir.path()).await);
    let otra_vez = EmbeddedJournal::open_in(dir.path()).await;
    assert!(
        matches!(otra_vez, EmbeddedJournal::Owned(_)),
        "soltar el primero libera el lock: {otra_vez:?}"
    );
}
