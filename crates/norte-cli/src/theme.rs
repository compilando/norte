//! `norte theme import` (spec 2026-09-11, F5): a VS Code theme converted into
//! `<config>/themes/<name>.toml`.
//!
//! The parser and the projection are pure and live in `norte_theme::vscode`;
//! here is what touches the disk: walking the `include` chain, writing the
//! file and, with `--use`, `[ui] theme`. Fully SYNC: `run` calls it from
//! `spawn_blocking` (rule 2).

use std::collections::HashSet;
use std::io::Read as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, anyhow, bail};
use clap::Subcommand;
use norte_theme::Theme;
use norte_theme::vscode::{self, VsCodeTheme};

/// How far `include` is followed. VS Code themes nest three deep (modern →
/// plus → vs); eight is generous and cuts off a pathological chain.
const MAX_INCLUDES: usize = 8;

/// Maximum size of a theme file. `dark_vs.json` is 9 KB and One Dark Pro is
/// 62 KB; an `include` pointing at `/dev/zero` must not be read to the end.
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

/// Runs a `norte theme …`. SYNC: reads and writes the disk.
pub(crate) fn run(cmd: &ThemeCmd) -> anyhow::Result<ExitCode> {
    let ThemeCmd::Import {
        file,
        name,
        usar,
        force,
    } = cmd;
    let config_dir = norte_config::user_config_dir()
        .ok_or_else(|| anyhow!(norte_i18n::t("cli-theme-err-no-config-dir")))?;
    let done = import(file, name.as_deref(), *force, &config_dir)?;
    println!(
        "{}",
        norte_i18n::ta(
            "cli-theme-imported",
            &[
                ("name", &done.name),
                ("path", &display_path(&done.path)),
                ("base", done.base),
            ],
        )
    );
    if !done.ignored.is_empty() {
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-theme-ignored",
                &[
                    ("count", &done.ignored.len().to_string()),
                    ("ids", &ids_for_display(&done.ignored)),
                ],
            )
        );
    }
    if *usar {
        let written = norte_config::persist_set(
            &config_dir,
            "ui",
            "theme",
            toml_edit::Value::from(done.name.as_str()),
        )
        .with_context(|| norte_i18n::t("cli-theme-err-use"))?;
        println!(
            "{}",
            norte_i18n::ta(
                "cli-theme-used",
                &[("name", &done.name), ("path", &display_path(&written)),],
            )
        );
    }
    Ok(ExitCode::SUCCESS)
}

/// What an import left written.
#[derive(Debug)]
pub(crate) struct Imported {
    pub(crate) name: String,
    pub(crate) path: PathBuf,
    pub(crate) base: &'static str,
    pub(crate) ignored: Vec<String>,
}

/// Imports `file` into `<config_dir>/themes/<name>.toml`.
pub(crate) fn import(
    file: &Path,
    name: Option<&str>,
    force: bool,
    config_dir: &Path,
) -> anyhow::Result<Imported> {
    let vs = read_chain(file)?;
    let name = match name {
        Some(n) => n.to_owned(),
        None => default_name(&vs, file),
    };
    if !norte_frontend::theme::is_theme_name(&name) {
        bail!(norte_i18n::t("cli-theme-err-name-invalid"));
    }
    // The resolver puts the presets FIRST: a `themes/nord.toml` would never
    // be read, and writing it would be an operation that does nothing without
    // saying so.
    if norte_theme::preset_source(&name).is_some() {
        bail!(norte_i18n::ta(
            "cli-theme-err-name-preset",
            &[("name", &name)]
        ));
    }

    let base_name = vs.base_or_default().preset();
    // Unreachable: both presets are embedded and `norte-theme` tests that
    // they parse. An error and not an `expect` because it costs nothing.
    let base = Theme::preset(base_name)
        .ok()
        .flatten()
        .ok_or_else(|| anyhow!("embedded preset {base_name} missing"))?;
    let mut theme = vscode::to_theme(&vs.colors, &base);
    theme.name = Some(name.clone());

    let dir = config_dir.join("themes");
    let path = dir.join(format!("{name}.toml"));
    if !force && path.symlink_metadata().is_ok() {
        bail!(norte_i18n::ta(
            "cli-theme-err-exists",
            &[("path", &display_path(&path))]
        ));
    }
    std::fs::create_dir_all(&dir).with_context(|| display_path(&dir))?;
    let content = format!("{}{}", header(file, base_name, &vs), theme.to_toml());
    write_atomic(&dir, &path, &name, &content, force)?;

    Ok(Imported {
        name,
        path,
        base: base_name,
        ignored: vs.ignored,
    })
}

/// Writes `content` to `path` so that a reader — the hot reload of an open
/// frontend — never sees a half-written theme.
///
/// A UNIQUELY named temp file created with `create_new` (it does not follow a
/// symlink someone left under that name, and two imports at once do not step
/// on each other), flushed to disk, and then: without `--force`, `hard_link`,
/// which fails atomically if the destination appeared between the check and
/// here; with `--force`, `rename`, which replaces — a symlink too, which
/// becomes a regular file. The temp file is removed no matter what happens.
fn write_atomic(
    dir: &Path,
    path: &Path,
    name: &str,
    content: &str,
    force: bool,
) -> anyhow::Result<()> {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let tmp = dir.join(format!(".{name}.{}.{unique}.tmp", std::process::id()));
    let result = place(&tmp, path, content, force);
    // After a `rename` it no longer exists and this simply fails: no matter.
    let _ = std::fs::remove_file(&tmp);
    result
}

/// The body of [`write_atomic`]: creates the temp file, flushes it and puts
/// it in place. Separate so `write_atomic` removes the temp file on EVERY
/// path.
fn place(tmp: &Path, path: &Path, content: &str, force: bool) -> anyhow::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(tmp)
        .with_context(|| display_path(tmp))?;
    f.write_all(content.as_bytes())
        .and_then(|()| f.sync_all())
        .with_context(|| display_path(tmp))?;
    drop(f);
    if force {
        std::fs::rename(tmp, path).with_context(|| display_path(path))
    } else {
        match std::fs::hard_link(tmp, path) {
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                bail!(norte_i18n::ta(
                    "cli-theme-err-exists",
                    &[("path", &display_path(path))]
                ))
            }
            other => other.with_context(|| display_path(path)),
        }
    }
}

/// Reads `file` and its `include` chain, each parent BELOW its child.
fn read_chain(file: &Path) -> anyhow::Result<VsCodeTheme> {
    let root = canonicalize_checked(file)?;
    let mut theme = read_one(&root)?;
    let mut seen = HashSet::from([root.clone()]);
    let mut current = root;
    while let Some(include) = theme.include.clone() {
        if seen.len() > MAX_INCLUDES {
            bail!(norte_i18n::ta(
                "cli-theme-err-depth",
                &[("max", &MAX_INCLUDES.to_string())]
            ));
        }
        // Relative to the file that names it, not to the working directory.
        let dir = current.parent().unwrap_or(Path::new("/"));
        let next = canonicalize_checked(&dir.join(&include))?;
        if !seen.insert(next.clone()) {
            bail!(norte_i18n::ta(
                "cli-theme-err-cycle",
                &[("path", &display_path(&next))]
            ));
        }
        let parent_theme = read_one(&next)?;
        theme.merge_under(parent_theme);
        current = next;
    }
    Ok(theme)
}

/// A path for a message: nothing the terminal will interpret. A file
/// name — and an `include`, written by whoever published the theme — can
/// carry ESC, a bidi override or a newline (rule 1).
fn display_path(path: &Path) -> String {
    norte_encoding::mask_terminal_hazards(&path.display().to_string())
}

/// The ignored ids, for stderr: they are JSON KEYS, so whoever published the
/// theme wrote them. Masked, each with a cap, and at most ten.
fn ids_for_display(ids: &[String]) -> String {
    const MAX_IDS: usize = 10;
    const MAX_CHARS: usize = 64;
    let mut out: Vec<String> = ids
        .iter()
        .take(MAX_IDS)
        .map(|id| {
            let mut short: String = id.chars().take(MAX_CHARS).collect();
            if id.chars().count() > MAX_CHARS {
                short.push('…');
            }
            norte_encoding::mask_terminal_hazards(&short)
        })
        .collect();
    if ids.len() > MAX_IDS {
        out.push("…".to_owned());
    }
    out.join(", ")
}

fn canonicalize_checked(path: &Path) -> anyhow::Result<PathBuf> {
    std::fs::canonicalize(path)
        .with_context(|| norte_i18n::ta("cli-theme-err-read", &[("path", &display_path(path))]))
}

/// A theme file: regular, size-capped, and JSONC.
fn read_one(path: &Path) -> anyhow::Result<VsCodeTheme> {
    let shown = display_path(path);
    let read_err = || norte_i18n::ta("cli-theme-err-read", &[("path", &shown)]);
    let meta = std::fs::metadata(path).with_context(read_err)?;
    if !meta.is_file() {
        bail!(read_err());
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|f| f.take(MAX_BYTES + 1).read_to_end(&mut bytes))
        .with_context(read_err)?;
    if bytes.len() as u64 > MAX_BYTES {
        bail!(norte_i18n::ta(
            "cli-theme-err-too-big",
            &[
                ("path", &shown),
                ("max", &(MAX_BYTES / 1024 / 1024).to_string())
            ]
        ));
    }
    // JSON is UTF-8, but a Windows editor saves UTF-16 with a BOM, and that
    // is also a theme. Only a BOM is honoured: without one, guessing the
    // encoding of a file that SHOULD be UTF-8 does more harm than good.
    let src = match norte_encoding::detect(&bytes) {
        norte_encoding::Detection::Text {
            encoding,
            bom: true,
        } => norte_encoding::decode(&bytes, encoding, true).text,
        norte_encoding::Detection::Text { bom: false, .. } => {
            String::from_utf8_lossy(&bytes).into_owned()
        }
        // NUL without a BOM: unmarked UTF-16, or a binary.
        norte_encoding::Detection::Binary => {
            bail!(norte_i18n::ta("cli-theme-err-binary", &[("path", &shown)]))
        }
    };
    vscode::parse(&src).with_context(|| norte_i18n::ta("cli-theme-err-parse", &[("path", &shown)]))
}

/// The JSON's `name` turned into a theme name, or the file's name.
fn default_name(vs: &VsCodeTheme, file: &Path) -> String {
    let from_json = vs.name.as_deref().map(slug).filter(|s| !s.is_empty());
    from_json.unwrap_or_else(|| {
        file.file_stem()
            .map(|s| slug(&s.to_string_lossy()))
            .unwrap_or_default()
    })
}

/// `"One Dark Pro"` → `one-dark-pro`: ASCII lowercase, everything else to
/// dashes.
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

/// Where the file comes from, said in the file itself: whoever opens it to
/// tweak it has to know it is not a preset nor something written by hand.
fn header(file: &Path, base: &str, vs: &VsCodeTheme) -> String {
    // A file name can carry a newline, and a newline inside a TOML comment
    // ends the comment; or a bidi override, which shows the reader a name
    // that is not real (rule 1: the name is bytes, and here it is only
    // shown).
    let origin = norte_encoding::mask_terminal_hazards(
        &file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let mut lines = vec![
        norte_i18n::t("cli-theme-header-imported"),
        norte_i18n::ta("cli-theme-header-source", &[("file", &origin)]),
        norte_i18n::ta("cli-theme-header-base", &[("base", base)]),
    ];
    if !vs.ignored.is_empty() {
        lines.push(norte_i18n::ta(
            "cli-theme-header-ignored",
            &[("count", &vs.ignored.len().to_string())],
        ));
    }
    let mut out = String::new();
    for line in lines {
        // A translation should not carry a newline, but if it does it must
        // not let text escape the comment.
        out.push_str("# ");
        out.push_str(&line.replace(['\n', '\r'], " "));
        out.push('\n');
    }
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_of_real_names() {
        assert_eq!(slug("One Dark Pro"), "one-dark-pro");
        assert_eq!(slug("Mi Tema: Noche"), "mi-tema-noche");
        assert_eq!(slug("  --Dracula--  "), "dracula");
        assert_eq!(slug("日本"), "");
    }

    #[test]
    fn the_header_does_not_let_a_newline_escape() {
        let c = header(
            Path::new("/x/mal\nname = 1.json"),
            "vscode-dark",
            &VsCodeTheme::default(),
        );
        assert!(Theme::from_toml(&c).is_ok(), "{c}");
        assert!(c.lines().all(|l| l.is_empty() || l.starts_with('#')), "{c}");
    }
}
