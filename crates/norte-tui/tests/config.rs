//! Tests de la config en capas (ADR 0007): precedencia, diagnóstico con
//! archivo, y claves desconocidas como error claro.

use norte_tui::config::{ConfigError, Layers, load};

fn dir_with(files: &[(&str, &str)]) -> tempfile::TempDir {
    let d = tempfile::tempdir().expect("tempdir");
    for (name, content) in files {
        std::fs::write(d.path().join(name), content).expect("write");
    }
    d
}

#[test]
fn defaults_sin_ninguna_capa() {
    let cfg = load(&Layers { dirs: vec![] }).expect("defaults");
    assert_eq!(
        cfg.preset, "orthodox",
        "default compilado (decisión 2026-07-10)"
    );
    assert!(cfg.keymap_layers.is_empty());
}

#[test]
fn el_ultimo_gana_por_campo_y_las_capas_de_keymap_se_acumulan() {
    let sistema = dir_with(&[
        ("norte.toml", "[keymap]\npreset = \"cua\"\n"),
        (
            "keymap.toml",
            "[pane]\nappend_keymap = [{ on = [\"x\"], run = \"app.quit\" }]\n",
        ),
    ]);
    let usuario = dir_with(&[("norte.toml", "[keymap]\npreset = \"vim\"\n")]);
    let proyecto = dir_with(&[(
        "keymap.toml",
        "[pane]\nprepend_keymap = [{ on = [\"z\"], run = \"cursor.top\" }]\n",
    )]);
    let layers = Layers {
        dirs: vec![
            sistema.path().to_path_buf(),
            usuario.path().to_path_buf(),
            proyecto.path().to_path_buf(),
        ],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(
        cfg.preset, "vim",
        "el preset del usuario pisa al del sistema"
    );
    assert_eq!(
        cfg.keymap_layers.len(),
        2,
        "las capas de keymap NO se pisan: se pliegan (ADR 0007)"
    );
}

#[test]
fn toml_roto_nombra_el_archivo() {
    let mala = dir_with(&[("norte.toml", "esto no es toml ===")]);
    match load(&Layers {
        dirs: vec![mala.path().to_path_buf()],
    }) {
        Err(ConfigError::Toml { path, .. }) => {
            assert!(
                path.ends_with("norte.toml"),
                "diagnóstico con archivo: {path:?}"
            );
        }
        other => panic!("esperaba Toml, fue {other:?}"),
    }
}

#[test]
fn clave_desconocida_es_error_claro() {
    let mala = dir_with(&[("norte.toml", "[keymap]\npresett = \"vim\"\n")]);
    match load(&Layers {
        dirs: vec![mala.path().to_path_buf()],
    }) {
        Err(ConfigError::Toml { path, .. }) => {
            assert!(path.ends_with("norte.toml"));
        }
        other => panic!("esperaba Toml (deny_unknown_fields), fue {other:?}"),
    }
}

#[test]
fn dir_sin_archivos_no_molesta() {
    let vacia = dir_with(&[]);
    let cfg = load(&Layers {
        dirs: vec![vacia.path().to_path_buf(), "/no/existe/en/absoluto".into()],
    })
    .expect("capas ausentes = defaults");
    assert_eq!(cfg.preset, "orthodox");
}

/// Regla 3 para el poll del watcher: detecta cambios y soltar el `Watch`
/// lo CANCELA limpio (el task suelta su sender → el canal se cierra).
#[tokio::test]
async fn el_polling_detecta_cambios_y_se_cancela_limpio() {
    let d = dir_with(&[("norte.toml", "[keymap]\npreset = \"vim\"\n")]);
    let layers = Layers {
        dirs: vec![d.path().to_path_buf()],
    };
    let (tx, mut rx) = tokio::sync::mpsc::channel(8);
    let watch = norte_tui::config::watch_polling(&layers, tx, std::time::Duration::from_millis(20));

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
