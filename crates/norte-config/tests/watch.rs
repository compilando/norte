//! Integration tests for the `watch` feature (ADR 0007): polling detects
//! changes and cleans up on drop (regla 3), and the polling snapshot covers
//! every layer file — including `openers.toml`.

use norte_config::{Layer, Layers, watch_polling};

fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    for (name, content) in files {
        std::fs::write(d.path().join(name), content).expect("write");
    }
    d
}

/// Regla 3 para el poll del watcher: detecta cambios y soltar el `Watch`
/// lo CANCELA limpio (el task suelta su sender → el canal se cierra).
#[tokio::test]
async fn el_polling_detecta_cambios_y_se_cancela_limpio() {
    let d = dir_with(&[("norte.toml", "[keymap]\npreset = \"vim\"\n")]);
    let layers = Layers {
        dirs: vec![(d.path().to_path_buf(), Layer::User)],
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let watch = watch_polling(&layers, tx, std::time::Duration::from_millis(20));

    // Deja tomar el snapshot base y cambia el archivo (mtime Y tamaño).
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    std::fs::write(
        d.path().join("norte.toml"),
        "[keymap]\npreset = \"orthodox\"\n",
    )
    .unwrap();
    let visto = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    assert!(
        matches!(visto, Ok(Some(()))),
        "el poll ve el cambio: {visto:?}"
    );

    drop(watch);
    // Tras cancelar, el task termina y suelta el sender: recv → None
    // (drenando los eventos que quedaran en vuelo).
    loop {
        match tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await {
            Ok(Some(())) => {}
            Ok(None) => break,
            Err(timeout) => {
                panic!("el task de polling no terminó tras soltar el Watch: {timeout}")
            }
        }
    }
}

/// Pin (TDD): la corrección aplicada al copiar `snapshot` a este crate
/// añadió `"openers.toml"` al array de archivos vigilados por el fallback de
/// polling — antes solo cubría `norte.toml`/`keymap.toml`, así que una
/// edición de `openers.toml` bajo polling puro (watcher nativo caído) se
/// perdía. Verificado por TDD: quitar `"openers.toml"` del array de
/// `snapshot` en `src/watch.rs` hace fallar este test (timeout, ningún tick
/// llega); restaurarlo lo pasa de nuevo.
#[tokio::test]
async fn el_polling_detecta_cambios_en_openers_toml() {
    let d = dir_with(&[(
        "openers.toml",
        "[[opener]]\nmime = \"text/*\"\ncommand = [\"less\", \"%f\"]\n",
    )]);
    let layers = Layers {
        dirs: vec![(d.path().to_path_buf(), Layer::User)],
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let watch = watch_polling(&layers, tx, std::time::Duration::from_millis(20));

    // Deja tomar el snapshot base y cambia el archivo (mtime Y tamaño).
    tokio::time::sleep(std::time::Duration::from_millis(60)).await;
    std::fs::write(
        d.path().join("openers.toml"),
        "[[opener]]\nmime = \"text/*\"\ncommand = [\"bat\", \"%f\"]\n",
    )
    .unwrap();
    let visto = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv()).await;
    assert!(
        matches!(visto, Ok(Some(()))),
        "el poll ve el cambio de openers.toml: {visto:?}"
    );

    drop(watch);
}
