//! Tests de la config en capas (ADR 0007): precedencia, diagnóstico con
//! archivo, y claves desconocidas como error claro.

use norte_tui::config::{ConfigError, Layer, Layers, load};
use norte_tui::keymap::{COMMANDS, Effective, Screen, presets};

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
        cfg.common.preset, "orthodox",
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
            (sistema.path().to_path_buf(), Layer::System),
            (usuario.path().to_path_buf(), Layer::User),
            (proyecto.path().to_path_buf(), Layer::Project),
        ],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(
        cfg.common.preset, "vim",
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
        dirs: vec![(mala.path().to_path_buf(), Layer::User)],
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
        dirs: vec![(mala.path().to_path_buf(), Layer::User)],
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
        dirs: vec![
            (vacia.path().to_path_buf(), Layer::User),
            ("/no/existe/en/absoluto".into(), Layer::Project),
        ],
    })
    .expect("capas ausentes = defaults");
    assert_eq!(cfg.common.preset, "orthodox");
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

/// ALTA (security review M4 Lua): `load` marca como PROYECTO el
/// `keymap.toml` de la capa `Layer::Project` (`./.norte`; deuda #75 cerrada:
/// el kind viaja POR DIR) y esa marca fluye hasta `Effective::build_for`,
/// que descarta sus bindings `lua:` (un repo hostil no rebindea teclas a
/// comandos Lua del usuario). La capa de usuario NO se marca.
#[test]
fn el_keymap_de_la_ultima_capa_se_marca_como_proyecto() {
    let binding = "[pane]\nprepend_keymap = [{ on = [\"j\"], run = \"lua:pwn\" }]\n";
    let usuario = dir_with(&[("keymap.toml", binding)]);
    let proyecto = dir_with(&[("keymap.toml", binding)]);
    let layers = Layers {
        dirs: vec![
            (usuario.path().to_path_buf(), Layer::User),
            (proyecto.path().to_path_buf(), Layer::Project),
        ],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(cfg.keymap_layers.len(), 2);
    assert!(!cfg.keymap_layers[0].is_project(), "capa de usuario");
    assert!(cfg.keymap_layers[1].is_project(), "última capa = proyecto");

    // Y el efecto de seguridad, de punta a punta: el lua: del PROYECTO se
    // descarta (contado); el de USUARIO sobrevive y gana la precedencia.
    let (_, preset) = &presets()[0];
    let eff = Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Browse)
        .expect("descartar no es error");
    assert_eq!(eff.discarded_lua_bindings(), 1, "solo el del proyecto");
    assert!(
        eff.bindings().iter().any(|(_, run)| *run == "lua:pwn"),
        "el binding de la capa de USUARIO sigue vivo"
    );
}

/// #95.2: `[archive]` se fusiona último-gana entre capas de CONFIANZA y la
/// capa de proyecto se IGNORA — un `./.norte/norte.toml` de un repo ajeno no
/// puede subir los límites anti-bomba justo donde viven los contenedores
/// hostiles (mismo criterio fail-closed que la hotlist).
#[test]
fn archive_limits_ultimo_gana_y_proyecto_no_los_toca() {
    let sistema = dir_with(&[(
        "norte.toml",
        "[archive]\nmax_entries = 1000\nmax_decompressed_bytes = 4096\n",
    )]);
    let usuario = dir_with(&[("norte.toml", "[archive]\nmax_entries = 50\n")]);
    let proyecto = dir_with(&[(
        "norte.toml",
        "[archive]\nmax_entries = 999999999\nmax_decompressed_bytes = 999999999\n",
    )]);
    let layers = Layers {
        dirs: vec![
            (sistema.path().to_path_buf(), Layer::System),
            (usuario.path().to_path_buf(), Layer::User),
            (proyecto.path().to_path_buf(), Layer::Project),
        ],
    };
    let cfg = load(&layers).expect("carga");
    assert_eq!(
        cfg.common.archive_max_entries,
        Some(50),
        "usuario pisa sistema"
    );
    assert_eq!(
        cfg.common.archive_max_decompressed_bytes,
        Some(4096),
        "campo no pisado conserva la capa inferior"
    );
}

/// Roadmap ítem 9, y las DOS mitades son la aserción: lo que este binario
/// diagnostica llega al FICHERO, y NO a stderr.
///
/// La primera versión de este test lanzaba `--version`, que sale treinta líneas
/// ANTES de que se instale el subscriber — así que el proceso bajo prueba no
/// instalaba ninguno y la aserción se cumplía sola, incluido si alguien cambiara
/// `init_to_file` por el `init` de la CLI, que es justo la regresión que decía
/// pinnear. Ahora se le da un `[ai]` roto, que es un aviso real por un camino
/// que sí carga config, instala el subscriber y sale sin abrir la TTY.
#[test]
fn el_frontend_de_terminal_loguea_al_fichero_y_no_a_la_pantalla() {
    let estado = tempfile::tempdir().expect("tmp");
    let config = tempfile::tempdir().expect("tmp");
    // Config VÁLIDA con un proveedor de IA que no resuelve: parsea (el tipo de
    // proveedor no se valida al leer, a propósito — lo rechaza la puerta de la
    // IA al usarlo, donde el diagnóstico puede nombrarlo), así que se llega a
    // instalar el subscriber y el aviso sale por `tracing::warn!`. Uno que no
    // parseara abortaría por stderr ANTES, que es correcto y no es esto.
    std::fs::write(
        config.path().join("norte.toml"),
        "[ai]\nenabled = true\nrename_provider = \"x\"\n\n\
         [ai.providers.x]\nkind = \"inventado\"\nmodel = \"m\"\n",
    )
    .expect("config");

    let salida = std::process::Command::new(env!("CARGO_BIN_EXE_ntc"))
        .arg("--pick")
        .arg(estado.path())
        .env("XDG_STATE_HOME", estado.path())
        .env("NORTE_CONFIG_DIR", config.path())
        .env("RUST_LOG", "warn")
        .output()
        .expect("ejecuta");

    // El proceso muere al no encontrar TTY, y ese error SÍ va a stderr a
    // propósito: es lo que le dice al usuario por qué no arrancó. Lo que no
    // puede aparecer ahí es el DIAGNÓSTICO, que es lo que rompería la pantalla
    // si hubiera pantalla.
    let stderr = String::from_utf8_lossy(&salida.stderr).into_owned();
    assert!(
        !stderr.contains("proveedor de IA no disponible"),
        "el aviso no puede salir por la pantalla: {stderr}"
    );

    // Y la otra mitad: el aviso ESTÁ, en el fichero.
    let logs = estado.path().join("norte").join("logs");
    let texto: String = std::fs::read_dir(&logs)
        .unwrap_or_else(|e| panic!("no hay directorio de logs en {logs:?}: {e}"))
        .flatten()
        .map(|f| std::fs::read_to_string(f.path()).unwrap_or_default())
        .collect();
    assert!(
        texto.contains("proveedor de IA no disponible"),
        "el aviso tiene que estar en el log: {texto}"
    );
}
