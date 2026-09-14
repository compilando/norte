//! `norte theme import` (spec 2026-09-11, F5): un tema de VS Code convertido
//! en `<config>/themes/<nombre>.toml`.
//!
//! El parser y la proyección son puros y viven en `norte_theme::vscode`; aquí
//! está lo que toca el disco: recorrer la cadena `include`, escribir el
//! fichero y, con `--use`, el `[ui] theme`. SYNC entero: `run` lo llama desde
//! `spawn_blocking` (regla 2).

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, anyhow, bail};
use clap::Subcommand;
use norte_theme::Theme;
use norte_theme::vscode::{self, VsCodeTheme};

/// Hasta dónde se sigue `include`. Los temas de VS Code anidan tres (modern →
/// plus → vs); ocho es holgado y corta una cadena patológica.
const MAX_INCLUDES: usize = 8;

/// Tamaño máximo de un fichero de tema. `dark_vs.json` son 9 KB y One Dark Pro
/// 62 KB; un `include` que apunta a `/dev/zero` no debe leerse hasta el final.
const MAX_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Subcommand, Clone)]
pub(crate) enum ThemeCmd {
    /// Importa un tema de VS Code (`.json`, con comentarios o sin ellos) a
    /// `<config>/themes/<nombre>.toml`, pintado sobre `vscode-dark` o
    /// `vscode-light` según su tipo. Sigue su cadena `include`
    Import {
        /// El fichero JSON del tema
        file: PathBuf,
        /// Nombre del tema; por defecto, el `name` del JSON o el del fichero
        #[arg(long)]
        name: Option<String>,
        /// Además, lo deja puesto en `[ui] theme`
        #[arg(long = "use")]
        usar: bool,
        /// Sustituye un tema con ese nombre si ya existe
        #[arg(long)]
        force: bool,
    },
}

/// Ejecuta un `norte theme …`. SYNC: lee y escribe el disco.
pub(crate) fn run(cmd: &ThemeCmd) -> anyhow::Result<ExitCode> {
    let ThemeCmd::Import {
        file,
        name,
        usar,
        force,
    } = cmd;
    let config_dir = norte_config::user_config_dir()
        .ok_or_else(|| anyhow!(norte_i18n::t("cli-theme-err-no-config-dir")))?;
    let hecho = import(file, name.as_deref(), *force, &config_dir)?;
    println!(
        "{}",
        norte_i18n::ta(
            "cli-theme-imported",
            &[
                ("name", &hecho.name),
                ("path", &ruta(&hecho.path)),
                ("base", hecho.base),
            ],
        )
    );
    if !hecho.ignored.is_empty() {
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-theme-ignored",
                &[
                    ("count", &hecho.ignored.len().to_string()),
                    ("ids", &ids_para_mostrar(&hecho.ignored)),
                ],
            )
        );
    }
    if *usar {
        let escrito = norte_config::persist_set(
            &config_dir,
            "ui",
            "theme",
            toml_edit::Value::from(hecho.name.as_str()),
        )
        .with_context(|| norte_i18n::t("cli-theme-err-use"))?;
        println!(
            "{}",
            norte_i18n::ta(
                "cli-theme-used",
                &[("name", &hecho.name), ("path", &ruta(&escrito)),],
            )
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// Lo que dejó escrito una importación.
#[derive(Debug)]
pub(crate) struct Importado {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) base: &'static str,
    pub(crate) ignored: Vec<String>,
}

/// Importa `file` a `<config_dir>/themes/<nombre>.toml`.
pub(crate) fn import(
    file: &Path,
    name: Option<&str>,
    force: bool,
    config_dir: &Path,
) -> anyhow::Result<Importado> {
    let vs = leer_cadena(file)?;
    let name = match name {
        Some(n) => n.to_owned(),
        None => nombre_por_defecto(&vs, file),
    };
    if !norte_frontend::theme::is_theme_name(&name) {
        bail!(norte_i18n::t("cli-theme-err-name-invalid"));
    }
    // El resolutor pone los presets PRIMERO: un `themes/nord.toml` no se
    // leería nunca, y escribirlo sería una operación que no hace nada sin
    // decirlo.
    if norte_theme::preset_source(&name).is_some() {
        bail!(norte_i18n::ta(
            "cli-theme-err-name-preset",
            &[("name", &name)]
        ));
    }

    let base_name = vs.base_or_default().preset();
    // Inalcanzable: los dos presets están embebidos y `norte-theme` testea
    // que parsean. Un error y no un `expect` porque no cuesta nada.
    let base = Theme::preset(base_name)
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("embedded preset {base_name} missing"))?;
    let mut tema = vscode::to_theme(&vs.colors, &base);
    tema.name = Some(name.clone());

    let dir = config_dir.join("themes");
    let path = dir.join(format!("{name}.toml"));
    if !force && path.symlink_metadata().is_ok() {
        bail!(norte_i18n::ta(
            "cli-theme-err-exists",
            &[("path", &ruta(&path))]
        ));
    }
    std::fs::create_dir_all(&dir).with_context(|| ruta(&dir))?;
    let contenido = format!("{}{}", cabecera(file, base_name, &vs), tema.to_toml());
    escribir(&dir, &path, &name, &contenido, force)?;

    Ok(Importado {
        name,
        path,
        base: base_name,
        ignored: vs.ignored,
    })
}

/// Escribe `contenido` en `path` sin que un lector —la recarga en caliente
/// de un frontend abierto— vea nunca un tema a medias.
///
/// Un temporal de nombre ÚNICO creado con `create_new` (no sigue un enlace
/// que alguien dejó con ese nombre, y dos importaciones a la vez no se pisan),
/// volcado a disco, y luego: sin `--force`, `hard_link`, que falla atómicamente
/// si el destino apareció entre la comprobación y aquí; con `--force`,
/// `rename`, que sustituye — también un enlace simbólico, que pasa a ser un
/// fichero. El temporal se borra pase lo que pase.
fn escribir(
    dir: &Path,
    path: &Path,
    name: &str,
    contenido: &str,
    force: bool,
) -> anyhow::Result<()> {
    let unico = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".{name}.{}.{unico}.tmp", std::process::id()));
    let resultado = colocar(&tmp, path, contenido, force);
    // Tras un `rename` ya no existe y esto falla sin más: da igual.
    let _ = std::fs::remove_file(&tmp);
    resultado
}

/// El cuerpo de [`escribir`]: crea el temporal, lo vuelca y lo pone en su
/// sitio. Aparte para que `escribir` borre el temporal en TODOS los caminos.
fn colocar(tmp: &Path, path: &Path, contenido: &str, force: bool) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .with_context(|| ruta(tmp))?;
    f.write_all(contenido.as_bytes())
        .and_then(|()| f.sync_all())
        .with_context(|| ruta(tmp))?;
    drop(f);
    if force {
        std::fs::rename(tmp, path).with_context(|| ruta(path))
    } else {
        match std::fs::hard_link(tmp, path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!(norte_i18n::ta(
                    "cli-theme-err-exists",
                    &[("path", &ruta(path))]
                ))
            }
            otro => otro.with_context(|| ruta(path)),
        }
    }
}

/// Lee `file` y su cadena `include`, cada padre DEBAJO de su hijo.
fn leer_cadena(file: &Path) -> anyhow::Result<VsCodeTheme> {
    let raiz = canonica(file)?;
    let mut tema = leer_uno(&raiz)?;
    let mut vistos = HashSet::from([raiz.clone()]);
    let mut actual = raiz;
    while let Some(include) = tema.include.clone() {
        if vistos.len() > MAX_INCLUDES {
            bail!(norte_i18n::ta(
                "cli-theme-err-depth",
                &[("max", &MAX_INCLUDES.to_string())]
            ));
        }
        // Relativo al fichero que lo nombra, no al directorio de trabajo.
        let dir = actual.parent().unwrap_or(Path::new("/"));
        let siguiente = canonica(&dir.join(&include))?;
        if !vistos.insert(siguiente.clone()) {
            bail!(norte_i18n::ta(
                "cli-theme-err-cycle",
                &[("path", &ruta(&siguiente))]
            ));
        }
        let padre = leer_uno(&siguiente)?;
        tema.merge_under(padre);
        actual = siguiente;
    }
    Ok(tema)
}

/// Una ruta para un mensaje: sin nada que la terminal interprete. Un nombre
/// de fichero —y un `include`, que escribe quien publicó el tema— puede
/// llevar ESC, un override bidi o un salto de línea (regla 1).
fn ruta(path: &Path) -> String {
    norte_encoding::mask_terminal_hazards(&path.display().to_string())
}

/// Los ids ignorados, para stderr: son CLAVES del JSON, así que las escribe
/// quien publicó el tema. Enmascarados, cada uno con tope, y como mucho diez.
fn ids_para_mostrar(ids: &[String]) -> String {
    const MAX_IDS: usize = 10;
    const MAX_CHARS: usize = 64;
    let mut out: Vec<String> = ids
        .iter()
        .take(MAX_IDS)
        .map(|id| {
            let mut corto: String = id.chars().take(MAX_CHARS).collect();
            if id.chars().count() > MAX_CHARS {
                corto.push('…');
            }
            norte_encoding::mask_terminal_hazards(&corto)
        })
        .collect();
    if ids.len() > MAX_IDS {
        out.push("…".to_owned());
    }
    out.join(", ")
}

fn canonica(path: &Path) -> anyhow::Result<PathBuf> {
    std::fs::canonicalize(path)
        .with_context(|| norte_i18n::ta("cli-theme-err-read", &[("path", &ruta(path))]))
}

/// Un fichero de tema: regular, con tope de tamaño, y JSONC.
fn leer_uno(path: &Path) -> anyhow::Result<VsCodeTheme> {
    let shown = ruta(path);
    let err_leer = || norte_i18n::ta("cli-theme-err-read", &[("path", &shown)]);
    let meta = std::fs::metadata(path).with_context(err_leer)?;
    if !meta.is_file() {
        bail!(err_leer());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .with_context(err_leer)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!(norte_i18n::ta(
            "cli-theme-err-too-big",
            &[
                ("path", &shown),
                ("max", &(MAX_BYTES / 1024 / 1024).to_string())
            ]
        ));
    }
    // JSON es UTF-8, pero un editor de Windows guarda UTF-16 con BOM, y eso
    // también es un tema. Solo se honra un BOM: sin él, adivinar el encoding
    // de un fichero que DEBERÍA ser UTF-8 hace más daño que bien.
    let src = match norte_encoding::detect(&bytes) {
        norte_encoding::Detection::Text {
            encoding,
            bom: true,
        } => norte_encoding::decode(&bytes, encoding, true).text,
        norte_encoding::Detection::Text { bom: false, .. } => {
            String::from_utf8_lossy(&bytes).into_owned()
        }
        // NUL sin BOM: UTF-16 sin marca, o un binario.
        norte_encoding::Detection::Binary => {
            bail!(norte_i18n::ta("cli-theme-err-binary", &[("path", &shown)]))
        }
    };
    vscode::parse(&src).with_context(|| norte_i18n::ta("cli-theme-err-parse", &[("path", &shown)]))
}

/// El `name` del JSON hecho nombre de tema, o el nombre del fichero.
fn nombre_por_defecto(vs: &VsCodeTheme, file: &Path) -> String {
    let desde_json = vs.name.as_deref().map(slug).filter(|s| !s.is_empty());
    desde_json.unwrap_or_else(|| {
        file.file_stem()
            .map(|s| slug(&s.to_string_lossy()))
            .unwrap_or_default()
    })
}

/// `"One Dark Pro"` → `one-dark-pro`: minúsculas ASCII, lo demás a guiones.
fn slug(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') {
            out.push('-');
        }
    }
    out.trim_matches('-').chars().take(64).collect()
}

/// De dónde sale el fichero, dicho en el propio fichero: quien lo abra para
/// retocarlo tiene que saber que no es un preset ni algo escrito a mano.
fn cabecera(file: &Path, base: &str, vs: &VsCodeTheme) -> String {
    // Un nombre de fichero puede llevar un salto de línea, y un salto dentro
    // de un comentario TOML termina el comentario; o un override bidi, que
    // enseña a quien lo lea un nombre que no es (regla 1: el nombre son
    // bytes, y aquí solo se enseña).
    let origen = norte_encoding::mask_terminal_hazards(
        &file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let mut lineas = vec![
        norte_i18n::t("cli-theme-header-imported"),
        norte_i18n::ta("cli-theme-header-source", &[("file", &origen)]),
        norte_i18n::ta("cli-theme-header-base", &[("base", base)]),
    ];
    if !vs.ignored.is_empty() {
        lineas.push(norte_i18n::ta(
            "cli-theme-header-ignored",
            &[("count", &vs.ignored.len().to_string())],
        ));
    }
    let mut out = String::new();
    for linea in lineas {
        // Una traducción no debería llevar un salto, pero si lo lleva no
        // puede sacar texto del comentario.
        out.push_str("# ");
        out.push_str(&linea.replace(['\n', '\r'], " "));
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_de_nombres_reales() {
        assert_eq!(slug("One Dark Pro"), "one-dark-pro");
        assert_eq!(slug("Mi Tema: Noche"), "mi-tema-noche");
        assert_eq!(slug("  --Dracula--  "), "dracula");
        assert_eq!(slug("日本"), "");
    }

    #[test]
    fn la_cabecera_no_deja_escapar_un_salto_de_linea() {
        let c = cabecera(
            Path::new("/x/mal\nname = 1.json"),
            "vscode-dark",
            &VsCodeTheme::default(),
        );
        assert!(Theme::from_toml(&c).is_ok(), "{c}");
        assert!(c.lines().all(|l| l.is_empty() || l.starts_with('#')), "{c}");
    }
}
