//! Scalar merge of `norte.toml` across layers (ADR 0007/0035) — the
//! [`CommonConfig`] every frontend loads at startup — plus the persistence
//! helpers that write back user preferences (theme, hotlist) with
//! `toml_edit` so comments and formatting survive.
//!
//! This module deliberately reads configuration with `std::fs`; using
//! providers would be circular because configuration selects how a frontend
//! starts.

use std::path::{Path, PathBuf};

use norte_proto::VPath;

use crate::dirs::{Layer, Layers};
use crate::schema::{self, ConfigError, DaemonMode, NorteToml, toml_diag};

/// Fija `[ui].theme = name` en el `norte.toml` del usuario, PRESERVANDO
/// comentarios y formato (`toml_edit`). Crea el fichero/directorio si no
/// existen. Devuelve la ruta escrita.
///
/// # Errors
/// [`std::io::Error`] si no hay dir de usuario, el TOML existente no parsea, o
/// falla el I/O.
pub fn persist_ui_theme(name: &str) -> std::io::Result<PathBuf> {
    let dir = crate::dirs::user_config_dir().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "sin directorio de config de usuario",
        )
    })?;
    persist_ui_theme_to(&dir, name)
}

/// Como [`persist_ui_theme`] pero en un `dir` explícito (sin depender del
/// entorno — la base testeable).
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea o falla el I/O.
pub fn persist_ui_theme_to(dir: &std::path::Path, name: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    let path = dir.join("norte.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // security review item 4 (C1, NIT F4): `toml_edit`'s parse
            // error `Display` quotes the offending document line — a
            // hostile `name`/`path` persisted earlier (#73 discipline)
            // would otherwise reach whatever shows this `io::Error`
            // (the TUI status bar). Name the file, never the content.
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} no parsea: TOML inválido; corrígelo o bórralo",
                    path.display()
                ),
            )
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    // Una tabla `[ui]` recién creada sería IMPLÍCITA (se emitiría como
    // `ui.theme = …` en vez de bajo `[ui]`): se crea EXPLÍCITA para que el
    // fichero nuevo tenga una sección legible; la ya existente se respeta.
    let ui = doc.as_table_mut().entry("ui").or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(false);
        toml_edit::Item::Table(t)
    });
    ui["theme"] = toml_edit::value(name);
    std::fs::write(&path, doc.to_string())?;
    Ok(path)
}

/// Añade (o reemplaza si `name` ya existe) una entrada `[[hotlist]]` en el
/// `norte.toml` de `dir`, PRESERVANDO comentarios y formato (mismo patrón
/// `toml_edit` que [`persist_ui_theme_to`]). `wire_path` se guarda TAL
/// CUAL — la validación a [`VPath`] ocurre al releer (`load`), no aquí:
/// persistir no debe rechazar un path que el propio `norte` todavía no
/// sabe interpretar (p.ej. un scheme nuevo de un provider futuro).
///
/// BLOQUEANTE: hace I/O de FS síncrono. El caller (T5) DEBE envolverla en
/// `tokio::task::spawn_blocking` — el runtime jamás se bloquea (regla 2),
/// mismo patrón que `persist_ui_theme` en `main.rs`.
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea, `hotlist` existe
/// pero no es un array de tablas, o falla el I/O.
pub fn persist_hotlist_add(dir: &Path, name: &str, wire_path: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    let path = dir.join("norte.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // security review item 4 (C1, NIT F4): `toml_edit`'s parse
            // error `Display` quotes the offending document line — a
            // hostile `name`/`path` persisted earlier (#73 discipline)
            // would otherwise reach whatever shows this `io::Error`
            // (the TUI status bar). Name the file, never the content.
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} no parsea: TOML inválido; corrígelo o bórralo",
                    path.display()
                ),
            )
        })?,
        Err(e) if e.kind() == ErrorKind::NotFound => toml_edit::DocumentMut::new(),
        Err(e) => return Err(e),
    };
    let arr = doc
        .as_table_mut()
        .entry("hotlist")
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| Error::new(ErrorKind::InvalidData, "`hotlist` no es un array de tablas"))?;
    // add REEMPLAZA si el name ya existe (spec: "add reemplaza si name
    // existe") — mismo comportamiento que renombrar/actualizar el favorito
    // sin dejar una entrada vieja huérfana.
    if let Some(existing) = arr
        .iter_mut()
        .find(|t| t.get("name").and_then(|v| v.as_str()) == Some(name))
    {
        existing["path"] = toml_edit::value(wire_path);
    } else {
        let mut t = toml_edit::Table::new();
        t["name"] = toml_edit::value(name);
        t["path"] = toml_edit::value(wire_path);
        arr.push(t);
    }
    std::fs::write(&path, doc.to_string())?;
    Ok(path)
}

/// Retira la entrada `[[hotlist]]` de nombre `name` del `norte.toml` de
/// `dir`, PRESERVANDO comentarios y formato. `name` inexistente (o
/// `norte.toml`/`hotlist` inexistentes) es NO-OP documentado: no hay nada
/// que borrar, no es un error — y crucialmente NO reescribe el fichero
/// (review MINOR-1: escribir sin cambios toca el mtime → el watcher de
/// `config::watch` lo confunde con una edición real y dispara un
/// hot-reload fantasma).
///
/// BLOQUEANTE: hace I/O de FS síncrono. El caller (T5) DEBE envolverla en
/// `tokio::task::spawn_blocking` — el runtime jamás se bloquea (regla 2),
/// mismo patrón que `persist_ui_theme` en `main.rs`.
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea o falla el I/O.
pub fn persist_hotlist_remove(dir: &Path, name: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    let path = dir.join("norte.toml");
    let mut doc = match std::fs::read_to_string(&path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            // security review item 4 (C1, NIT F4): `toml_edit`'s parse
            // error `Display` quotes the offending document line — a
            // hostile `name`/`path` persisted earlier (#73 discipline)
            // would otherwise reach whatever shows this `io::Error`
            // (the TUI status bar). Name the file, never the content.
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} no parsea: TOML inválido; corrígelo o bórralo",
                    path.display()
                ),
            )
        })?,
        // No-op documentado: nada que borrar.
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(path),
        Err(e) => return Err(e),
    };
    // Solo se escribe si `retain` REALMENTE quitó algo — comparar
    // longitudes antes/después en vez de escribir incondicionalmente.
    if let Some(arr) = doc
        .as_table_mut()
        .get_mut("hotlist")
        .and_then(toml_edit::Item::as_array_of_tables_mut)
    {
        let before = arr.len();
        arr.retain(|t| t.get("name").and_then(|v| v.as_str()) != Some(name));
        if arr.len() != before {
            std::fs::write(&path, doc.to_string())?;
        }
    }
    Ok(path)
}

/// Una entrada de hotlist YA fusionada y validada por [`load`]. `target` es
/// `Err` si el `path` de `norte.toml` no parsea como [`VPath`] — la entrada
/// se CONSERVA (se muestra con badge de error en el popup, T5) en vez de
/// tumbar la carga entera: la hotlist es data del usuario, no config
/// estructural (spec 2026-07-18, decisión 3). La clave de error es
/// ESTABLE (`"err-invalid-path"`, no el mensaje crudo del parser de
/// `VPath`): el popup la traduce vía Fluent y un path hostil (bidi,
/// kilométrico) jamás llega intacto a la barra (misma cautela que #73).
#[derive(Debug, Clone)]
pub struct HotlistItem {
    /// Nombre mostrado.
    pub name: String,
    /// Destino ya parseado, o la clave de error estable.
    pub target: Result<VPath, String>,
}

/// Clave de error ESTABLE para un `path` de hotlist que no parsea como
/// [`VPath`] (ver doc de [`HotlistItem`]).
const ERR_INVALID_PATH: &str = "err-invalid-path";

/// Valida `entry.path` a [`VPath`] y lo fusiona en `items`: si ya hay una
/// entrada con el mismo `name`, la reemplaza — sea de una capa ANTERIOR
/// (la capa posterior gana, igual que el resto de la config), sea de un
/// `[[hotlist]]` PREVIO dentro de la MISMA capa (TOML no impide repetir
/// `name` en un array de tablas; `load` llama a esta función una vez por
/// entrada, en orden de aparición, así que la ÚLTIMA gana también
/// intra-capa). Conserva la posición original para que el orden del popup
/// no salte al editar solo el `path` de un favorito ya existente. Si no
/// existía, se añade al final. Las claves (`name`) comparan byte-exactas
/// SIN normalizar (la identidad jamás se normaliza); twins NFC/NFD conviven
/// como filas distintas — decisión consciente.
fn merge_hotlist_entry(items: &mut Vec<HotlistItem>, entry: schema::HotlistEntry) {
    let target = VPath::parse(&entry.path).map_err(|_| ERR_INVALID_PATH.to_owned());
    if let Some(existing) = items.iter_mut().find(|it| it.name == entry.name) {
        existing.target = target;
    } else {
        items.push(HotlistItem {
            name: entry.name,
            target,
        });
    }
}

/// Quick-search behaviour of `/` (`[ui] quick_search`). The frontend maps
/// this onto its own navigation mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum QuickSearch {
    /// Narrow the listing (default).
    #[default]
    Filter,
    /// Move the cursor without changing the listing.
    Jump,
}

/// `[ai]` already merged across layers and validated (ADR 0035 decision 3:
/// scalars last-present-wins; `denied_prefixes` union; providers merge by
/// name, later layer wins).
#[derive(Debug, Clone, Default)]
pub struct AiSettings {
    /// AI enabled (default false).
    pub enabled: bool,
    /// Local-only mode (default false).
    pub local_only: bool,
    /// Validated denied prefixes (union of all non-project layers).
    pub denied_prefixes: Vec<norte_proto::VPath>,
    /// Provider selected for rename.
    pub rename_provider: Option<String>,
    /// Providers by name (`BTreeMap` keeps deterministic order).
    pub providers: std::collections::BTreeMap<String, crate::schema::AiProviderEntry>,
}

/// The merged `norte.toml` scalars — everything that is NOT a frontend-only
/// pass (keymap layers, openers). Core consumers read `archive_*`/`ai`;
/// frontends wrap this in their own loaded-config type.
#[derive(Debug, Clone)]
pub struct CommonConfig {
    /// Effective keymap preset (last-wins; compiled default).
    pub preset: String,
    /// `[ui] lang` (last-wins; None = environment).
    pub ui_lang: Option<String>,
    /// `[ui] theme` (last-wins; None = default preset).
    pub ui_theme: Option<String>,
    /// `[ui] quick_search`, validated (invalid value = load error).
    pub quick_search: QuickSearch,
    /// `[daemon] mode` (last-wins; None = embedded; never from Project —
    /// fail-closed, review MAJOR-1). Startup only.
    pub daemon_mode: Option<crate::schema::DaemonMode>,
    /// `[daemon] socket` (last-wins; None = OS default; never from Project —
    /// fail-closed, review MAJOR-1).
    pub daemon_socket: Option<std::path::PathBuf>,
    /// Hotlist merged from every layer except Project.
    pub hotlist: Vec<HotlistItem>,
    /// `[archive] max_entries` (last-wins per field; never from Project).
    pub archive_max_entries: Option<u64>,
    /// `[archive] max_decompressed_bytes` (last-wins; never from Project).
    pub archive_max_decompressed_bytes: Option<u64>,
    /// `[archive] max_nesting` (#56, last-wins; never from Project).
    pub archive_max_nesting: Option<usize>,
    /// `[ai]` merged (never from Project).
    pub ai: AiSettings,
    /// Files that participated (watcher + diagnostics).
    pub sources: Vec<std::path::PathBuf>,
}

/// Merges one layer's `[ai]` section (already filtered to non-Project by the
/// caller) into `ai`: scalars last-present-wins, `denied_prefixes` is a
/// UNION (each entry validated as a [`VPath`]), providers merge by name with
/// the later layer winning. Extracted out of [`load`] to stay under
/// clippy's line-count cap (ADR 0035 decision 3: the fail-closed Project
/// carve-out lives in the caller).
///
/// # Errors
/// [`ConfigError::Toml`] if a `denied_prefixes` entry does not parse as a
/// [`VPath`].
fn merge_ai_layer(
    ai: &mut AiSettings,
    a: crate::schema::AiSection,
    norte: &Path,
) -> Result<(), ConfigError> {
    if let Some(v) = a.enabled {
        ai.enabled = v;
    }
    if let Some(v) = a.local_only {
        ai.local_only = v;
    }
    if let Some(v) = a.rename_provider {
        ai.rename_provider = Some(v);
    }
    for (i, p) in a.denied_prefixes.iter().enumerate() {
        let vp = VPath::parse(p).map_err(|_| ConfigError::Toml {
            path: norte.to_path_buf(),
            // Same #73 caution as quick_search: never quote the raw
            // (possibly hostile) value in the diagnostic — the 1-based
            // index is enough to locate the offending entry.
            message: format!(
                "[ai] denied_prefixes: entry {} does not parse as a VPath",
                i + 1
            ),
        })?;
        if !ai.denied_prefixes.contains(&vp) {
            ai.denied_prefixes.push(vp);
        }
    }
    ai.providers.extend(a.providers);
    Ok(())
}

/// Loads and merges every layer (ADR 0007/0035).
///
/// # Errors
/// [`ConfigError`] naming the offending file; an ABSENT layer is not an
/// error.
pub fn load(layers: &Layers) -> Result<CommonConfig, ConfigError> {
    let mut preset: Option<String> = None;
    let mut ui_lang: Option<String> = None;
    let mut ui_theme: Option<String> = None;
    let mut quick_search = QuickSearch::default();
    let mut daemon_mode: Option<DaemonMode> = None;
    let mut daemon_socket: Option<PathBuf> = None;
    let mut hotlist: Vec<HotlistItem> = Vec::new();
    let mut archive_max_entries: Option<u64> = None;
    let mut archive_max_decompressed_bytes: Option<u64> = None;
    let mut archive_max_nesting: Option<usize> = None;
    let mut ai = AiSettings::default();
    let mut sources = Vec::new();
    for (dir, kind) in &layers.dirs {
        let norte = dir.join("norte.toml");
        if let Some(raw) = schema::read_optional(&norte)? {
            let parsed: NorteToml = toml::from_str(&raw).map_err(|e| ConfigError::Toml {
                path: norte.clone(),
                message: toml_diag(&raw, &e),
            })?;
            if let Some(p) = parsed.keymap.preset {
                preset = Some(p);
            }
            if let Some(l) = parsed.ui.lang {
                ui_lang = Some(l);
            }
            if let Some(th) = parsed.ui.theme {
                ui_theme = Some(th);
            }
            if let Some(qs) = parsed.ui.quick_search {
                quick_search = match qs.as_str() {
                    "filter" => QuickSearch::Filter,
                    "jump" => QuickSearch::Jump,
                    // Mensaje SIN citar el valor crudo (misma cautela que
                    // `toml_diag`, #73): un TOML hostil puede meter
                    // bidi/kilométrico en cualquier string, y este es un
                    // campo de dos valores válidos — no hace falta
                    // reflejar el resto para que el diagnóstico sea claro.
                    _ => {
                        return Err(ConfigError::Toml {
                            path: norte,
                            message: "[ui] quick_search inválido: solo se admite «filter» o «jump»"
                                .to_owned(),
                        });
                    }
                };
            }
            // `[daemon]` is NOT honored from Project either (review MAJOR-1):
            // a foreign repo must not redirect the core transport to an
            // attacker-controlled socket — same fail-closed carve-out as
            // `[archive]`/`[ai]`/hotlist.
            if *kind != Layer::Project {
                if let Some(m) = parsed.daemon.mode {
                    daemon_mode = Some(m);
                }
                if let Some(sock) = parsed.daemon.socket {
                    daemon_socket = Some(sock);
                }
            }
            // La hotlist se acumula de TODAS las capas MENOS la de
            // proyecto (deuda #75 cerrada: el kind viaja POR DIR, ya no se
            // infiere por posición). Un `./.norte/norte.toml` de un repo
            // ajeno no debe poder inyectar favoritos en la sesión del
            // usuario (spec 2026-07-18, decisión 3). Los ESCALARES de UI
            // (quick_search, theme, lang) SÍ se honran desde proyecto: son
            // config estructural de presentación (coherente con theme), no
            // data que dirija navegación como la hotlist.
            if *kind != Layer::Project {
                for entry in parsed.hotlist {
                    merge_hotlist_entry(&mut hotlist, entry);
                }
            }
            // `[archive]` (#95.2) TAMPOCO se honra desde proyecto: son
            // límites de SEGURIDAD anti-bomba — un `./.norte/norte.toml` de
            // un repo ajeno no debe poder SUBIRLOS y desarmar la protección
            // justo donde viven los contenedores hostiles (mismo criterio
            // fail-closed que la hotlist).
            if *kind != Layer::Project {
                if let Some(n) = parsed.archive.max_entries {
                    archive_max_entries = Some(n);
                }
                if let Some(b) = parsed.archive.max_decompressed_bytes {
                    archive_max_decompressed_bytes = Some(b);
                }
                if let Some(n) = parsed.archive.max_nesting {
                    archive_max_nesting = Some(n);
                }
            }
            // `[ai]` (ADR 0035 decisión 3) TAMPOCO se honra desde proyecto:
            // un `./.norte/norte.toml` de un repo ajeno no debe poder
            // habilitar la IA ni redirigir sus proveedores — mismo
            // carve-out fail-closed que `[archive]`/hotlist.
            if *kind != Layer::Project {
                merge_ai_layer(&mut ai, parsed.ai, &norte)?;
            }
            sources.push(norte);
        }
    }
    Ok(CommonConfig {
        preset: preset.unwrap_or_else(|| schema::DEFAULT_PRESET.to_owned()),
        ui_lang,
        ui_theme,
        quick_search,
        daemon_mode,
        daemon_socket,
        hotlist,
        archive_max_entries,
        archive_max_decompressed_bytes,
        archive_max_nesting,
        ai,
        sources,
    })
}

/// Tests de historial+hotlist (spec 2026-07-18, navTC T2) + `[ai]` (ADR
/// 0035): mod nuevo junto a `toml_diag_tests` de `schema.rs` (no
/// reutilizarlo — ese mod es solo del diagnóstico compacto de `#73`).
#[cfg(test)]
mod hotlist_tests {
    use super::*;

    #[test]
    fn hotlist_round_trip_preservando_comentarios() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# mi config\n[ui]\ntheme = \"nord\" # tema\n",
        )
        .unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///home/o/work").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("# mi config"), "comentarios intactos: {s}");
        assert!(s.contains("[[hotlist]]"), "{s}");
        persist_hotlist_remove(dir.path(), "trabajo").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(!s.contains("trabajo"), "{s}");
    }

    #[test]
    fn hotlist_add_reemplaza_si_el_nombre_ya_existe() {
        let dir = tempfile::tempdir().unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///a").unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///b").unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(
            s.matches("trabajo").count(),
            1,
            "una sola entrada, no duplicada: {s}"
        );
        assert!(s.contains("file:///b"), "{s}");
        assert!(!s.contains("file:///a"), "{s}");
    }

    #[test]
    fn hotlist_remove_de_nombre_inexistente_es_no_op() {
        let dir = tempfile::tempdir().unwrap();
        persist_hotlist_add(dir.path(), "trabajo", "file:///a").unwrap();
        let path = dir.path().join("norte.toml");
        // Comentario a mano: si el no-op reescribiera el fichero, toml_edit
        // podría reformatearlo igual — la prueba fuerte no es "no falla",
        // es "el CONTENIDO no cambia ni un byte" (review MINOR-1: mtime es
        // flaky por granularidad del FS, el contenido no).
        let mut s = std::fs::read_to_string(&path).unwrap();
        s.push_str("# nota manual\n");
        std::fs::write(&path, &s).unwrap();
        let before = std::fs::read_to_string(&path).unwrap();
        // No debe fallar aunque "fantasma" no exista (documentado: no-op).
        persist_hotlist_remove(dir.path(), "fantasma").unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert_eq!(before, after, "no-op no reescribe: contenido byte-idéntico");
        assert!(after.contains("trabajo"), "{after}");
    }

    #[test]
    fn hotlist_remove_sin_seccion_hotlist_es_no_op_y_no_reescribe() {
        // `norte.toml` existe pero SIN `[[hotlist]]` en absoluto: el no-op
        // tampoco debe tocar el fichero (mismo MINOR-1).
        let dir = tempfile::tempdir().unwrap();
        let content = "# sin hotlist\n[ui]\ntheme = \"nord\"\n";
        std::fs::write(dir.path().join("norte.toml"), content).unwrap();
        persist_hotlist_remove(dir.path(), "lo-que-sea").unwrap();
        let after = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(content, after, "sin `hotlist`: contenido intacto");
    }

    #[test]
    fn hotlist_se_carga_de_todas_las_capas_menos_proyecto() {
        // Dos dirs: capa `User` con una entrada, capa `Project` con otra —
        // la de proyecto NO debe entrar (spec: "un repo ajeno no inyecta
        // favoritos"), y la de usuario sí.
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"casa\"\npath = \"file:///home/o\"\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[[hotlist]]\nname = \"repo-ajeno\"\npath = \"file:///tmp/x\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(
            cfg.hotlist.len(),
            1,
            "solo la de usuario: {:?}",
            cfg.hotlist
        );
        assert_eq!(cfg.hotlist[0].name, "casa");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///home/o").unwrap()
        );
    }

    #[test]
    fn hotlist_entrada_invalida_degrada_por_entrada() {
        // Un path que no parsea como VPath (falta scheme) no tumba la
        // carga: la entrada sobrevive con `target = Err(...)`, y las demás
        // entradas de la misma capa se cargan con normalidad.
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"rota\"\npath = \"no-es-un-path-wire\"\n\n\
             [[hotlist]]\nname = \"sana\"\npath = \"file:///ok\"\n",
        )
        .unwrap();
        // La entrada va en una capa `User` (que SÍ aporta hotlist); la capa
        // `Project` (un repo ajeno) se añade vacía para comprobar que su
        // ausencia de favoritos no altera el resultado.
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("la carga NO falla por una entrada rota");
        assert_eq!(cfg.hotlist.len(), 2);
        let rota = cfg.hotlist.iter().find(|h| h.name == "rota").unwrap();
        assert_eq!(
            rota.target.as_ref().err().map(String::as_str),
            Some(ERR_INVALID_PATH)
        );
        let sana = cfg.hotlist.iter().find(|h| h.name == "sana").unwrap();
        assert!(sana.target.is_ok());
    }

    #[test]
    fn hotlist_nombre_duplicado_entre_capas_la_capa_posterior_gana() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[[hotlist]]\nname = \"trabajo\"\npath = \"file:///viejo\"\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"trabajo\"\npath = \"file:///nuevo\"\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.hotlist.len(), 1, "mismo nombre, una entrada");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///nuevo").unwrap(),
            "la capa posterior (usuario) gana sobre sistema"
        );
    }

    /// review MINOR-2: el dedup por `name` también aplica DENTRO de la
    /// MISMA capa — TOML no impide repetir `[[hotlist]] name = "..."` dos
    /// veces en el mismo array; la última aparición gana (ver rustdoc de
    /// `merge_hotlist_entry`).
    #[test]
    fn hotlist_nombre_duplicado_dentro_de_la_misma_capa_la_ultima_aparicion_gana() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[[hotlist]]\nname = \"trabajo\"\npath = \"file:///viejo\"\n\n\
             [[hotlist]]\nname = \"trabajo\"\npath = \"file:///nuevo\"\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.hotlist.len(), 1, "mismo nombre intra-capa, una entrada");
        assert_eq!(
            cfg.hotlist[0].target.as_ref().unwrap(),
            &VPath::parse("file:///nuevo").unwrap(),
            "la última aparición dentro de la capa gana"
        );
    }

    /// Pin encoding BAJA-1a: un `name` hostil (comilla, salto de línea, un
    /// `[[hotlist]]` embebido y un override bidi) sobrevive el round-trip
    /// add → load BYTE-IDÉNTICO como UNA sola entrada — `toml_edit` escapa,
    /// jamás inyecta TOML — y esa misma clave la retira con remove.
    #[test]
    fn hotlist_round_trip_name_hostil_byte_identico() {
        let usuario = tempfile::tempdir().unwrap();
        let name = "fa\"vo\n[[hotlist]]\u{202E}rito";
        persist_hotlist_add(usuario.path(), name, "file:///x").unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("el name hostil no rompe el TOML");
        assert_eq!(cfg.hotlist.len(), 1, "UNA entrada, sin inyección");
        assert_eq!(cfg.hotlist[0].name, name, "name byte-idéntico");
        persist_hotlist_remove(usuario.path(), name).unwrap();
        let cfg = load(&layers).expect("carga tras remove");
        assert!(cfg.hotlist.is_empty(), "la clave hostil retira su entrada");
    }

    /// Pin encoding BAJA-1b: el wire de un `VPath` con segmento no-UTF8
    /// (0xFF 0xFE) round-tripea add → load con `target` Ok y bytes exactos.
    #[test]
    fn hotlist_round_trip_path_no_utf8_bytes_exactos() {
        let usuario = tempfile::tempdir().unwrap();
        let vp = VPath::parse("file:///%FF%FE").unwrap();
        persist_hotlist_add(usuario.path(), "bin", &vp.to_wire()).unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        let layers = Layers {
            dirs: vec![
                (usuario.path().to_path_buf(), Layer::User),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        let target = cfg.hotlist[0].target.as_ref().expect("target Ok");
        assert_eq!(target, &vp);
        assert_eq!(
            target.file_name().unwrap().as_bytes(),
            &[0xFF, 0xFE],
            "los bytes crudos sobreviven el round-trip por TOML"
        );
    }

    #[test]
    fn quick_search_valores_validos() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nquick_search = \"jump\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.quick_search, QuickSearch::Jump);
    }

    #[test]
    fn quick_search_default_es_filter() {
        let cfg = load(&Layers { dirs: vec![] }).expect("carga");
        assert_eq!(cfg.quick_search, QuickSearch::Filter);
    }

    #[test]
    fn quick_search_valor_invalido_es_error_de_carga() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nquick_search = \"vuela\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let err = load(&layers).expect_err("config rota es error (ADR 0007)");
        assert!(matches!(err, ConfigError::Toml { .. }));
    }

    /// ADR 0035: [ai] merges across layers — scalars last-wins, denied
    /// prefixes UNION (a system deny survives a user layer) AND deduped (a
    /// prefix repeated across layers is not a distinct entry).
    #[test]
    fn ai_merge_escalares_ultimo_gana_y_denied_union() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[ai]\nenabled = true\nlocal_only = true\n\
             denied_prefixes = [\"file:///etc\", \"file:///shared\"]\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ai]\nlocal_only = false\n\
             denied_prefixes = [\"file:///home/u/secret\", \"file:///shared\"]\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert!(cfg.ai.enabled, "absent in user layer: inherits system");
        assert!(!cfg.ai.local_only, "present in user layer: user wins");
        let etc = VPath::parse("file:///etc").unwrap();
        let secreto = VPath::parse("file:///home/u/secret").unwrap();
        let compartido = VPath::parse("file:///shared").unwrap();
        assert_eq!(
            cfg.ai.denied_prefixes.len(),
            3,
            "UNION deduped: 3 distinct entries, `file:///shared` not doubled"
        );
        assert!(
            cfg.ai.denied_prefixes.contains(&etc),
            "system deny survives"
        );
        assert!(cfg.ai.denied_prefixes.contains(&secreto), "user deny added");
        assert!(
            cfg.ai.denied_prefixes.contains(&compartido),
            "shared deny present exactly once"
        );
    }

    /// [ai] from the project layer is ignored fail-closed — a hostile repo
    /// must not enable AI, declare a provider, nor add a denied prefix.
    #[test]
    fn ai_de_proyecto_se_ignora() {
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[ai]\nenabled = true\ndenied_prefixes = [\"file:///x\"]\n\n\
             [ai.providers.p]\nkind = \"ollama\"\nmodel = \"m\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(proyecto.path().to_path_buf(), Layer::Project)],
        };
        let cfg = load(&layers).expect("carga");
        assert!(!cfg.ai.enabled);
        assert!(cfg.ai.providers.is_empty(), "project provider ignored");
        assert!(
            cfg.ai.denied_prefixes.is_empty(),
            "project denied_prefixes ignored"
        );
    }

    /// ADR 0035: providers merge BY NAME — a later layer redeclaring an
    /// existing name replaces just that entry, an untouched name from a
    /// lower layer survives, and `rename_provider` (a plain scalar) is
    /// last-present-wins independent of the providers map.
    #[test]
    fn ai_providers_merge_por_nombre_capa_posterior_gana() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[ai]\nrename_provider = \"x\"\n\n\
             [ai.providers.x]\nkind = \"ollama\"\nmodel = \"viejo\"\n\n\
             [ai.providers.y]\nkind = \"ollama\"\nmodel = \"solo-sistema\"\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ai]\nrename_provider = \"y\"\n\n\
             [ai.providers.x]\nkind = \"ollama\"\nmodel = \"nuevo\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ai.providers.len(), 2, "both names survive");
        assert_eq!(
            cfg.ai.providers.get("x").unwrap().model,
            "nuevo",
            "user layer redeclares x: later layer wins"
        );
        assert_eq!(
            cfg.ai.providers.get("y").unwrap().model,
            "solo-sistema",
            "y untouched by user layer: survives"
        );
        assert_eq!(
            cfg.ai.rename_provider,
            Some("y".to_owned()),
            "scalar last-present-wins, independent of the providers map"
        );
    }

    /// Review MAJOR-1: `[daemon]` from the project layer must NOT be
    /// honored — a hostile repo must not redirect the core transport (mode
    /// or socket) to an attacker-controlled endpoint.
    #[test]
    fn daemon_de_proyecto_se_ignora() {
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[daemon]\nmode = \"daemon\"\nsocket = \"/tmp/evil.sock\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(proyecto.path().to_path_buf(), Layer::Project)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.daemon_mode, None, "project mode ignored");
        assert_eq!(cfg.daemon_socket, None, "project socket ignored");
    }

    /// Security review item 4 (C1, NIT F4): the persist helpers' "existing
    /// TOML doesn't parse" error must name the file but never echo
    /// `toml_edit`'s parse-error `Display`, which quotes the offending
    /// document line — a hostile line persisted earlier (#73 discipline)
    /// must not resurface verbatim in whatever shows this `io::Error` (the
    /// TUI status bar). Pinned across all three persist helpers with a
    /// planted line containing a bidi override + an embedded fake TOML
    /// header, deliberately broken syntax so the parse fails.
    #[test]
    fn persist_helpers_no_citan_el_error_crudo_de_toml_edit() {
        let hostile = "not toml \u{202E}[[hotlist]]\u{202C} = [unterminated\n";
        for helper in ["theme", "hotlist_add", "hotlist_remove"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("norte.toml"), hostile).unwrap();
            let err = match helper {
                "theme" => persist_ui_theme_to(dir.path(), "nord").unwrap_err(),
                "hotlist_add" => persist_hotlist_add(dir.path(), "n", "file:///x").unwrap_err(),
                _ => persist_hotlist_remove(dir.path(), "n").unwrap_err(),
            };
            let msg = err.to_string();
            assert!(
                !msg.contains("hotlist") && !msg.contains('\u{202E}'),
                "{helper}: error must not echo the hostile document content: {msg:?}"
            );
            assert!(
                msg.contains("norte.toml"),
                "{helper}: error should still name the offending file: {msg:?}"
            );
        }
    }
}
