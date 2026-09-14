//! `norte theme import` de punta a punta: el binario lee un tema de VS Code
//! con su cadena `include` y deja un TOML que el resolutor de los frontends
//! encuentra por nombre.

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use norte_theme::{Role, Theme};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/themes")
        .join(name)
}

/// El sujeto es el binario, nunca la configuración de quien corre la suite.
fn norte(config: &Path) -> Command {
    let mut c = Command::cargo_bin("norte").expect("binario norte compilado");
    c.env("NORTE_CONFIG_DIR", config);
    c.env("NORTE_LANG", "en");
    c
}

/// Importar produce un TOML que PARSEA, resuelve todos los roles del núcleo
/// y lleva los colores del hijo Y del padre: el viaje entero, no solo el
/// parser.
#[test]
fn importar_produce_un_tema_completo_y_resoluble() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "noche"])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "import falló: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    // El color que no es color se DICE, no se traga.
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("badge.background"), "{err}");

    let escrito = std::fs::read_to_string(config.path().join("themes/noche.toml")).unwrap();
    let t = Theme::from_toml(&escrito).expect("el TOML escrito parsea");
    assert_eq!(t.name.as_deref(), Some("noche"));
    for &role in Role::CORE {
        let s = t.style(role);
        assert!(
            s.fg.is_some() || s.bg.is_some(),
            "{role:?} sin color tras importar"
        );
    }
    // Del hijo, que gana al padre en `editor.background`.
    assert_eq!(t.style(Role::Background).bg.unwrap().to_hex(), "#101820");
    assert_eq!(t.style(Role::FocusBorder).fg.unwrap().to_hex(), "#ff8800");
    // Del padre, que el hijo no define.
    assert_eq!(
        t.style(Role::PaneBackground).bg.unwrap().to_hex(),
        "#0a1018"
    );
    // Con alfa: compuesto sobre el fondo, no el blanco opaco.
    assert_ne!(
        t.style(Role::ScrollbarSlider).bg.unwrap().to_hex(),
        "#ffffff"
    );

    // Y el resolutor de los frontends lo encuentra por NOMBRE.
    let resuelto =
        norte_frontend::theme::resolve_theme_in(Some("noche"), Some(config.path())).unwrap();
    assert_eq!(resuelto.name.as_deref(), Some("noche"));
}

/// Sin `--name`, el nombre sale del `name` del JSON.
#[test]
fn el_nombre_sale_del_json() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(config.path().join("themes/mi-tema-noche.toml").is_file());
}

/// Un nombre que ya es preset embebido se RECHAZA: el resolutor pone los
/// presets primero, así que el fichero nunca se leería y escribirlo sería
/// una operación que no hace nada sin decirlo.
#[test]
fn importar_con_el_nombre_de_un_preset_se_rechaza() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "nord"])
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "debería rechazar un nombre de preset"
    );
    assert!(!config.path().join("themes/nord.toml").exists());
}

/// Un nombre que el resolutor trataría como ruta tampoco se escribe.
#[test]
fn un_nombre_que_es_ruta_se_rechaza() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "../fuera"])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(!config.path().join("fuera.toml").exists());
}

/// No se pisa un tema existente sin `--force`; con él, sí.
#[test]
fn no_sobrescribe_sin_force() {
    let config = tempfile::tempdir().unwrap();
    let temas = config.path().join("themes");
    std::fs::create_dir_all(&temas).unwrap();
    std::fs::write(temas.join("noche.toml"), "name = \"mío\"\n").unwrap();

    let sin = norte(config.path())
        .args(["theme", "import"])
        .arg(fixture("hijo.jsonc"))
        .args(["--name", "noche"])
        .output()
        .unwrap();
    assert!(!sin.status.success());
    assert_eq!(
        std::fs::read_to_string(temas.join("noche.toml")).unwrap(),
        "name = \"mío\"\n",
        "el tema del usuario sigue intacto"
    );

    let con = norte(config.path())
        .args(["theme", "import", "--force", "--name", "noche"])
        .arg(fixture("hijo.jsonc"))
        .output()
        .unwrap();
    assert!(
        con.status.success(),
        "{}",
        String::from_utf8_lossy(&con.stderr)
    );
    assert!(
        std::fs::read_to_string(temas.join("noche.toml"))
            .unwrap()
            .contains("name = \"noche\"")
    );
}

/// Un tema que se incluye a sí mismo (a → b → a) es un ERROR, no un cuelgue.
#[test]
fn un_ciclo_de_include_es_error() {
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "ciclo"])
        .arg(fixture("ciclo-a.json"))
        .timeout(std::time::Duration::from_secs(30))
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("loops back"), "{err}");
    assert!(!config.path().join("themes/ciclo.toml").exists());
}

/// `--use` deja el tema puesto en `[ui] theme` y respeta lo que ya había en
/// `norte.toml`, comentarios incluidos.
#[test]
fn use_escribe_ui_theme_sin_perder_comentarios() {
    let config = tempfile::tempdir().unwrap();
    std::fs::write(
        config.path().join("norte.toml"),
        "# mi config\n[ui]\ntheme = \"nord\"\n",
    )
    .unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--use", "--name", "noche"])
        .arg(fixture("hijo.jsonc"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let toml = std::fs::read_to_string(config.path().join("norte.toml")).unwrap();
    assert!(toml.contains("# mi config"), "{toml}");
    assert!(toml.contains("theme = \"noche\""), "{toml}");
}

/// Una cadena de `n` includes en un directorio temporal: `t0` → `t1` → … → `tn`.
fn cadena_de_includes(dir: &Path, n: usize) -> PathBuf {
    for i in 0..=n {
        let include = if i < n {
            format!(r#""include": "t{}.json", "#, i + 1)
        } else {
            String::new()
        };
        std::fs::write(
            dir.join(format!("t{i}.json")),
            format!(r#"{{ {include}"colors": {{}} }}"#),
        )
        .unwrap();
    }
    dir.join("t0.json")
}

/// Ocho includes se siguen; nueve no: el tope corta una cadena patológica
/// sin rechazar las de verdad (VS Code anida tres).
#[test]
fn el_tope_de_includes() {
    let ocho = tempfile::tempdir().unwrap();
    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "ocho"])
        .arg(cadena_de_includes(ocho.path(), 8))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );

    let nueve = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "nueve"])
        .arg(cadena_de_includes(nueve.path(), 9))
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("more than 8 includes"));
}

/// ¿Lleva `s` algo que una terminal interpretaría o que falsea lo que se lee?
fn tiene_peligro(s: &str) -> bool {
    // El salto de línea que cierra cada mensaje es del CLI, no del tema.
    s.chars()
        .filter(|c| *c != '\n')
        .any(norte_encoding::is_terminal_hazard)
}

/// Las claves de `colors` las escribe quien publicó el tema, y las que no
/// son color se NOMBRAN en stderr. Una clave con ESC+OSC reescribiría el
/// título de la terminal de quien importa: sale enmascarada.
#[test]
fn una_clave_hostil_no_llega_cruda_a_la_terminal() {
    let payload = norte_testkit::corpus::hostile_runs()
        .into_iter()
        .find(|r| r.id == "run_osc_title_injection")
        .expect("fixture del corpus")
        .run;
    let dir = tempfile::tempdir().unwrap();
    let tema = dir.path().join("hostil.json");
    let json = serde_json::json!({ "type": "dark", "colors": { payload: "no es un color" } });
    std::fs::write(&tema, json.to_string()).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "hostil"])
        .arg(&tema)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("app.quit"), "la clave se nombra: {err}");
    assert!(!tiene_peligro(&err), "ESC/BEL crudos en stderr: {err:?}");
}

/// Un nombre de fichero con RIGHT-TO-LEFT OVERRIDE ni rompe la cabecera del
/// TOML ni la falsea para quien la lea, ni sale crudo en los mensajes.
#[cfg(unix)]
#[test]
fn un_nombre_de_fichero_hostil_se_enmascara_en_cabecera_y_mensajes() {
    use std::os::unix::ffi::OsStrExt as _;
    let nombre = norte_testkit::corpus::hostile_names()
        .into_iter()
        .find(|n| n.id == "rtl_override")
        .expect("fixture del corpus");
    let dir = tempfile::tempdir().unwrap();
    let tema = dir.path().join(std::ffi::OsStr::from_bytes(&nombre.bytes));
    std::fs::copy(fixture("padre.json"), &tema).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "rlo"])
        .arg(&tema)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(!tiene_peligro(&String::from_utf8_lossy(&out.stdout)));
    let escrito = std::fs::read_to_string(config.path().join("themes/rlo.toml")).unwrap();
    let cabecera: String = escrito.lines().take_while(|l| l.starts_with('#')).collect();
    assert!(!tiene_peligro(&cabecera), "{cabecera:?}");
}

/// Un tema guardado en UTF-16 con BOM —lo que exportan algunos editores de
/// Windows— es un tema, no «no es un tema de VS Code».
#[test]
fn un_tema_en_utf16_con_bom_se_importa() {
    let texto = std::fs::read_to_string(fixture("padre.json")).unwrap();
    let mut bytes = vec![0xFF, 0xFE];
    for unidad in texto.encode_utf16() {
        bytes.extend_from_slice(&unidad.to_le_bytes());
    }
    let dir = tempfile::tempdir().unwrap();
    let tema = dir.path().join("utf16.json");
    std::fs::write(&tema, bytes).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "u16"])
        .arg(&tema)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let t =
        Theme::from_toml(&std::fs::read_to_string(config.path().join("themes/u16.toml")).unwrap())
            .unwrap();
    assert_eq!(
        t.style(Role::PaneBackground).bg.unwrap().to_hex(),
        "#0a1018"
    );
}

/// Sin BOM, UTF-16 tiene NULs y no se puede distinguir de un binario: el
/// error lo dice, en vez de afirmar que el fichero no es un tema.
#[test]
fn utf16_sin_bom_da_un_error_que_nombra_el_encoding() {
    let mut bytes = Vec::new();
    for unidad in "{\"colors\":{}}".encode_utf16() {
        bytes.extend_from_slice(&unidad.to_le_bytes());
    }
    let dir = tempfile::tempdir().unwrap();
    let tema = dir.path().join("sin-bom.json");
    std::fs::write(&tema, bytes).unwrap();

    let config = tempfile::tempdir().unwrap();
    let out = norte(config.path())
        .args(["theme", "import", "--name", "x"])
        .arg(&tema)
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("UTF-16"), "{err}");
}
