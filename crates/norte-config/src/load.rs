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
/// entorno — la base testeable). Envoltorio fino sobre [`persist_set`] (S2):
/// mantiene su propia firma (`name: &str`, no `toml_edit::Value`) porque es
/// el punto de entrada histórico, pero la escritura la hace enteramente
/// `persist_set(dir, "ui", "theme", …)` — prueba de comportamiento en el
/// mismo `#[test]` de antes (`hotlist_tests`), sin cambios.
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea o falla el I/O.
pub fn persist_ui_theme_to(dir: &std::path::Path, name: &str) -> std::io::Result<PathBuf> {
    persist_set(dir, "ui", "theme", toml_edit::Value::from(name))
}

/// Fija `[section] key = value` en el `norte.toml` de `dir`, PRESERVANDO
/// comentarios y formato (`toml_edit`) — la forma GENÉRICA detrás de
/// [`persist_ui_theme_to`] (S2, mismo patrón EXACTO: lee-o-crea un
/// `DocumentMut`, la tabla de sección nace EXPLÍCITA — jamás implícita, para
/// que un fichero nuevo lea limpio — fija `key`, escribe). Crea el
/// dir/fichero si no existen. `value` ya viene tipado por el caller
/// (`toml_edit::Value`): un string se escapa solo (mismo mecanismo que
/// `toml_edit::value(name)` usaba antes aquí), un bool/int se escriben nativos.
///
/// CONTRATO de forma (revisión S, I1): cada `[section]` de `norte.toml` debe
/// ser una tabla plana (`[section]` real o `section = { .. }` inline) —
/// NUNCA un escalar (`ui = 3`) ni un array-of-tables (`[[ui]]`). `load` solo
/// AVISA si una sección tiene una forma inesperada (degrada esa sección,
/// sigue arrancando); este escritor, en cambio, debe RECHAZAR explícitamente
/// una sección con forma escalar — indexar un `toml_edit::Item` escalar por
/// clave (`item[key] = ..`) no crea nada: `panic!("index not found")` (la
/// implementación de `IndexMut` de `toml_edit` para un `Item::Value` no
/// tabla devuelve `None` internamente y el operador de índice lo
/// `.expect()`). Alcanzable con una config editada a mano entre sesiones (el
/// TUI solo lo AVISA en el reload, no lo bloquea) — un `panic` aquí tumbaría
/// el hilo de fondo (GUI: se lleva el proceso; TUI: `JoinError` silencioso
/// tras un `spawn_blocking`). Se comprueba con `Item::is_table_like` (el
/// mismo criterio que usa el `IndexMut` interno de `toml_edit` para decidir
/// si puede indexar) — así el guard nunca rechaza una forma que la propia
/// librería aceptaría.
///
/// Desde #116 la escritura es SEGURA entre procesos: toma el lock advisory
/// `norte.toml.lock` ANTES de leer (puede bloquear mientras otro proceso
/// persiste — ver `lock_user_toml`) y reemplaza el fichero vía tmp +
/// `rename` atómico (`write_user_toml`): un lector concurrente jamás ve
/// un fichero a medias. Aplica a TODA la familia `persist_*`.
///
/// # Errors
/// [`std::io::Error`] si no hay dir de usuario, el TOML existente no parsea,
/// la sección existente no es una tabla (forma inesperada, ver el CONTRATO
/// arriba), o falla el I/O.
pub fn persist_set(
    dir: &std::path::Path,
    section: &str,
    key: &str,
    value: toml_edit::Value,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    // #116: lock ANTES de leer — el RMW entero es la sección crítica.
    let _lock = lock_user_toml(dir)?;
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
    // Guard de forma (revisión S, I1) — ANTES de tocar nada: una sección
    // existente que no sea tabla (escalar, array-of-tables…) indexaría en
    // pánico más abajo (ver el CONTRATO del rustdoc). `get` no crea nada
    // (a diferencia de `entry`), así que este chequeo es de solo lectura.
    if let Some(existing) = doc.as_table().get(section)
        && !existing.is_table_like()
    {
        return Err(Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}: [{section}] no es una tabla (forma inesperada); corrígelo o bórralo",
                path.display()
            ),
        ));
    }
    // Una tabla `[section]` recién creada sería IMPLÍCITA (se emitiría como
    // `section.key = …` en vez de bajo `[section]`): se crea EXPLÍCITA para
    // que el fichero nuevo tenga una sección legible; la ya existente se
    // respeta.
    let table = doc.as_table_mut().entry(section).or_insert_with(|| {
        let mut t = toml_edit::Table::new();
        t.set_implicit(false);
        toml_edit::Item::Table(t)
    });
    table[key] = toml_edit::Item::Value(value);
    write_user_toml(&path, &doc)?;
    Ok(path)
}

/// El `sort` a persistir (#108 7a) — espejo consciente de [`SortChoice`]:
/// este writer serializa EXACTAMENTE el vocabulario que `load` parsea en
/// `parse_sort_section` (`column` = `name`|`size`|`mtime`, `dir` =
/// `asc`|`desc`, `dirs_first` bool), pineado por el round-trip test de al
/// lado. Toma strings/bools crudos (no [`SortChoice`]) porque el caller es
/// un frontend que ya habla el vocabulario Display de las columnas.
#[derive(Debug, Clone, Copy)]
pub struct PersistSort<'a> {
    /// `"name"` | `"size"` | `"mtime"` (vocabulario cerrado del load).
    pub column: &'a str,
    /// `true` = descendente (se serializa como `dir = "desc"`).
    pub descending: bool,
    /// Directorios primero.
    pub dirs_first: bool,
}

/// Escribe la selección del picker de columnas (#108 7a) en el `norte.toml`
/// de `dir`, PRESERVANDO comentarios y formato (`toml_edit`, mismo patrón
/// que [`persist_set`]): `scheme = None` fija `[ui.columns]` `default` +
/// `sort`; `Some(s)` fija `[ui.columns.scheme.<s>]` `columns` + `sort`.
/// Primera escritura ANIDADA del persistidor — `persist_set` solo sabe de
/// `[section] key = escalar` — y primer valor ARRAY: las tablas intermedias
/// nacen implícitas (no emiten cabeceras vacías), la hoja nace EXPLÍCITA
/// (un `[ui.columns]` legible, mismo criterio que `persist_set`).
///
/// CONTRATO de forma: el guard `is_table_like` de [`persist_set`] se aplica
/// nivel a nivel ANTES de mutar nada — un nivel escalar (`ui = 3`) indexado
/// panicaría (ver el CONTRATO de `persist_set`) y tumbaría el hilo de fondo
/// del caller. BLOQUEANTE: I/O de FS síncrono — el caller DEBE envolverla
/// en `spawn_blocking` (regla 2), mismo patrón que `persist_hotlist_add`.
///
/// CONTRATO: `scheme` debe ser un scheme de `VPath` VALIDADO
/// (`[a-z][a-z0-9+.-]*`), como entregan los callers actuales
/// (`VPath::scheme()`). `toml_edit` escapa la clave igualmente — no hay
/// inyección — pero un string arbitrario viajaría en el `Display` del
/// error del guard de forma, convirtiéndolo en carrier de contenido
/// hostil (#73).
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea, un nivel existente de
/// la cadena no es una tabla (forma inesperada), o falla el I/O.
pub fn persist_columns(
    dir: &std::path::Path,
    scheme: Option<&str>,
    ids: &[String],
    sort: PersistSort<'_>,
) -> std::io::Result<PathBuf> {
    std::fs::create_dir_all(dir)?;
    // #116: lock ANTES de leer — el RMW entero es la sección crítica.
    let _lock = lock_user_toml(dir)?;
    let path = dir.join("norte.toml");
    let mut doc = open_user_toml(&path)?;
    let segs: Vec<&str> = match scheme {
        None => vec!["ui", "columns"],
        Some(s) => vec!["ui", "columns", "scheme", s],
    };
    let t = nested_table_mut(&mut doc, &path, &segs)?;
    let mut arr = toml_edit::Array::new();
    for id in ids {
        // Ids TAL CUAL (set abierto — `attr:`/`plugin:`/no-parseables):
        // `toml_edit` escapa, jamás inyecta TOML (pin en `persist_set`).
        arr.push(id.as_str());
    }
    // La clave de la lista difiere por diseño del schema (#108 block 4):
    // `default` bajo `[ui.columns]`, `columns` en un override de scheme.
    let list_key = if scheme.is_none() {
        "default"
    } else {
        "columns"
    };
    t.insert(
        list_key,
        toml_edit::Item::Value(toml_edit::Value::Array(arr)),
    );
    let mut sort_tbl = toml_edit::InlineTable::new();
    sort_tbl.insert("column", sort.column.into());
    sort_tbl.insert("dir", if sort.descending { "desc" } else { "asc" }.into());
    sort_tbl.insert("dirs_first", sort.dirs_first.into());
    t.insert(
        "sort",
        toml_edit::Item::Value(toml_edit::Value::InlineTable(sort_tbl)),
    );
    write_user_toml(&path, &doc)?;
    Ok(path)
}

/// Lock advisory cross-process del `norte.toml` (#116): `flock`/`LockFileEx`
/// sobre el hermano DEDICADO `norte.toml.lock` — jamás sobre el propio
/// `norte.toml`: la escritura atómica lo reemplaza por `rename` (inode
/// nuevo) y un lock sobre el inode viejo no excluiría al siguiente
/// escritor. Los escritores lo toman ANTES de leer: la sección crítica es
/// el ciclo lee-modifica-escribe ENTERO (lost update cerrado, también
/// entre procesos — GUI + TUI sobre el mismo fichero). Se libera al soltar
/// el guard (cerrar el descriptor); el SO lo suelta igualmente si el
/// proceso muere — no hay locks rancios tras un crash.
struct UserTomlLock {
    /// Mantiene vivo el descriptor bloqueado; drop = cerrar = unlock.
    _file: std::fs::File,
}

/// Toma (bloqueando) el lock de escritores de `dir`. BLOQUEANTE como el
/// resto del persistidor (regla 2: el caller ya envuelve en
/// `spawn_blocking`); los escritores son cortos — retener el lock
/// milisegundos — y no hay locks anidados, así que la espera no acota:
/// un peer VIVO pero colgado reteniéndolo es el único caso patológico
/// (decisión: bloquear simple; el SO libera al morir el proceso).
///
/// La apertura es SIN truncar (review #116 MAJOR-1): `File::create`
/// (`CREATE_ALWAYS`) sobre un lockfile que otro proceso tiene bajo
/// `LockFileEx` puede FALLAR en Windows (sharing/lock violation) en vez de
/// llegar al `lock()` que espera — exactamente la contención GUI+TUI que
/// este lock cierra. En POSIX truncar un fichero vacío era inocuo, pero la
/// forma canónica de abrir un lockfile es no tocarlo jamás.
fn lock_user_toml(dir: &Path) -> std::io::Result<UserTomlLock> {
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("norte.toml.lock"))?;
    file.lock()?;
    Ok(UserTomlLock { _file: file })
}

/// Escritura ATÓMICA del `norte.toml` (#116): tmp hermano + `rename`
/// (atómico en POSIX; `std::fs::rename` reemplaza también en Windows). Un
/// lector concurrente ve el fichero viejo o el nuevo COMPLETO — jamás un
/// truncado a medias que parsee "bien" y del que un escritor posterior
/// reconstruya el documento perdiendo secciones ajenas. `sync_all` antes
/// del rename evita la ventana fichero-vacío-tras-crash; el rename mismo
/// puede perderse en un corte de luz (sin fsync del dir, a propósito):
/// reaparece la config VIEJA — consistente, solo rancia. Un tmp huérfano
/// de un crash es inocuo: la siguiente escritura (mismo nombre, bajo el
/// lock) lo pisa. Los permisos del fichero existente se COPIAN al tmp
/// (review #116 MINOR-2: sin esto un `chmod 600` del usuario se ensanchaba
/// al umask en el reemplazo). Limitación Windows conocida: un proceso
/// externo (editor, AV) con `norte.toml` abierto sin `FILE_SHARE_DELETE`
/// hace fallar el rename con sharing violation — la persistencia falla
/// visible, sin retry (los lectores de Rust std comparten en modo full).
fn write_user_toml(path: &Path, doc: &toml_edit::DocumentMut) -> std::io::Result<()> {
    use std::io::Write;
    let tmp = path.with_file_name("norte.toml.tmp");
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(doc.to_string().as_bytes())?;
        match std::fs::metadata(path) {
            Ok(meta) => f.set_permissions(meta.permissions())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

/// Lee (o crea, si no existe) el `norte.toml` de `path` como documento
/// `toml_edit`, con el error de parseo SANEADO — el `Display` de
/// `toml_edit` cita la línea ofensora, y un `name`/`path` hostil
/// persistido antes llegaría a quien muestre este `io::Error` (la status
/// bar, #73): se nombra el fichero, jamás el contenido. Compartido por los
/// escritores de columnas (`persist_columns`/`persist_column_format`).
fn open_user_toml(path: &std::path::Path) -> std::io::Result<toml_edit::DocumentMut> {
    use std::io::{Error, ErrorKind};
    match std::fs::read_to_string(path) {
        Ok(s) => s.parse::<toml_edit::DocumentMut>().map_err(|_| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{} no parsea: TOML inválido; corrígelo o bórralo",
                    path.display()
                ),
            )
        }),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(toml_edit::DocumentMut::new()),
        Err(e) => Err(e),
    }
}

/// Camina/crea la cadena `segs` de tablas bajo `doc` (#108 7a/7b — DOS
/// callers: [`persist_columns`] y [`persist_column_format`]) y devuelve la
/// hoja mutable.
///
/// Guard de forma nivel a nivel, de SOLO LECTURA y ANTES de tocar nada
/// (mismo criterio `is_table_like` que `persist_set`): un nivel que se
/// corta (no existe) hace segura la creación de todo lo de debajo. La
/// mutación camina con `entry` sobre `TableLike` — NO con el operador de
/// índice, cuyo `IndexMut` en este `toml_edit` materializa los niveles que
/// falten como tablas INLINE con dotted-keys (`ui = { columns.default = … }`),
/// ilegible para un fichero editable a mano. Cada nivel nuevo nace
/// `Item::Table`: intermedios IMPLÍCITOS (sin cabecera propia), la hoja
/// EXPLÍCITA (`[ui.columns]` legible, mismo criterio que `persist_set`);
/// una hoja `Item::Table` ya existente se fuerza a explícita; una inline
/// (`ui = { columns = {…} }`) se respeta tal cual.
fn nested_table_mut<'d>(
    doc: &'d mut toml_edit::DocumentMut,
    path: &std::path::Path,
    segs: &[&str],
) -> std::io::Result<&'d mut dyn toml_edit::TableLike> {
    use std::io::{Error, ErrorKind};
    let forma = |hasta: usize| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}: [{}] no es una tabla (forma inesperada); corrígelo o bórralo",
                path.display(),
                segs[..=hasta].join(".")
            ),
        )
    };
    {
        let mut nivel: &dyn toml_edit::TableLike = doc.as_table();
        for (i, seg) in segs.iter().enumerate() {
            let Some(item) = nivel.get(seg) else { break };
            if !item.is_table_like() {
                return Err(forma(i));
            }
            let Some(t) = item.as_table_like() else {
                // Inalcanzable: `is_table_like` acaba de pasar (es
                // literalmente `as_table_like().is_some()`).
                break;
            };
            nivel = t;
        }
    }
    // Seguro tras el guard: ya no hay nivel existente no-tabla.
    let mut t: &mut dyn toml_edit::TableLike = doc.as_table_mut();
    for (i, seg) in segs.iter().enumerate() {
        let es_hoja = i + 1 == segs.len();
        let item = t.entry(seg).or_insert_with(|| {
            let mut nt = toml_edit::Table::new();
            nt.set_implicit(!es_hoja);
            toml_edit::Item::Table(nt)
        });
        if es_hoja && let Some(tab) = item.as_table_mut() {
            tab.set_implicit(false);
        }
        // Inalcanzable el `Err`: el guard ya rechazó todo nivel existente
        // no-tabla y los nuevos nacen `Item::Table` — pero un `Err` limpio
        // antes que un `unwrap` (regla 6).
        t = item.as_table_like_mut().ok_or_else(|| forma(i))?;
    }
    Ok(t)
}

/// Fija (o crea) el `format` del `[[ui.columns.spec]]` de id `id` (#108
/// 7b): reemplazo POR ID que PRESERVA el resto de campos de la entrada
/// (`header`/`width`/`align`) y los comentarios del fichero — el
/// precedente `ArrayOfTables` de [`persist_hotlist_add`], incluido su
/// guard de forma (`spec` existente que no sea array de tablas = error
/// limpio, jamás pánico).
///
/// CONTRATO: `id` viene del vocabulario del picker (ids Display de
/// builtins) y `format` del vocabulario CERRADO de formatos ya validado
/// contra su columna; `toml_edit` escapa igualmente. BLOQUEANTE: I/O de FS
/// síncrono — el caller DEBE envolverla en `spawn_blocking` (regla 2).
///
/// # Errors
/// [`std::io::Error`] si el TOML existente no parsea, un nivel de
/// `[ui.columns]` no es tabla, `spec` existe con otra forma, o falla el
/// I/O.
pub fn persist_column_format(dir: &Path, id: &str, format: &str) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    // #116: lock ANTES de leer — el RMW entero es la sección crítica.
    let _lock = lock_user_toml(dir)?;
    let path = dir.join("norte.toml");
    let mut doc = open_user_toml(&path)?;
    let t = nested_table_mut(&mut doc, &path, &["ui", "columns"])?;
    let arr = t
        .entry("spec")
        .or_insert_with(|| toml_edit::Item::ArrayOfTables(toml_edit::ArrayOfTables::new()))
        .as_array_of_tables_mut()
        .ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "{}: `spec` no es un array de tablas (forma inesperada); corrígelo o bórralo",
                    path.display()
                ),
            )
        })?;
    // TODAS las entradas del id, no solo la primera (MAJOR revisión 7b): el
    // loader fusiona duplicados intra-capa LAST-wins por campo — escribir
    // solo la primera dejaría la sesión y el fichero divergentes tras un
    // reload (el duplicado tardío pisa lo recién guardado con el toast ya
    // enseñado). Actualizarlas todas auto-sana la divergencia.
    let mut alguna = false;
    for tb in arr
        .iter_mut()
        .filter(|tb| tb.get("id").and_then(|v| v.as_str()) == Some(id))
    {
        tb["format"] = toml_edit::value(format);
        alguna = true;
    }
    if !alguna {
        let mut tb = toml_edit::Table::new();
        tb["id"] = toml_edit::value(id);
        tb["format"] = toml_edit::value(format);
        arr.push(tb);
    }
    write_user_toml(&path, &doc)?;
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
    // #116: lock ANTES de leer — el RMW entero es la sección crítica.
    let _lock = lock_user_toml(dir)?;
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
    write_user_toml(&path, &doc)?;
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
    // #116: lock ANTES de leer. Este writer no crea el dir (solo borra):
    // dir inexistente = nada que borrar = el mismo no-op documentado que
    // el `norte.toml` ausente de abajo.
    let _lock = match lock_user_toml(dir) {
        Ok(l) => l,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(path),
        Err(e) => return Err(e),
    };
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
            write_user_toml(&path, &doc)?;
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

/// `[ui] confirm_quit` behaviour (S2): whether `app.quit` opens a
/// confirmation modal before closing. Each frontend interprets `Auto`'s
/// "pending work" against its own model (TUI: active task-board rows; GUI:
/// tasks/marks) — this type only carries the mode, not the predicate.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ConfirmQuit {
    /// Confirm only when work is pending (the behavior before this setting
    /// existed, preserved as the default). Default.
    #[default]
    Auto,
    /// Always confirm, even with nothing pending.
    Always,
    /// Never confirm; `app.quit` closes immediately.
    Never,
}

impl ConfirmQuit {
    /// The wire string this variant round-trips from/to (`"auto"`,
    /// `"always"`, `"never"`) — used by the settings registry to display the
    /// current value.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Always => "always",
            Self::Never => "never",
        }
    }
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

/// `[ui.columns] sort` resuelto y VALIDADO (#108): vocabulario cerrado —
/// un valor inválido es error de carga con la ruta culpable (patrón
/// `quick_search`). El default reproduce el orden histórico.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SortChoice {
    /// Columna de orden.
    pub column: SortColumnKey,
    /// Dirección.
    pub descending: bool,
    /// Directorios primero.
    pub dirs_first: bool,
}

impl Default for SortChoice {
    fn default() -> Self {
        Self {
            column: SortColumnKey::Name,
            descending: false,
            dirs_first: true,
        }
    }
}

/// Columna de orden del vocabulario CERRADO de config (#108). El frontend
/// la mapea a su `SortSpec`; separada para no invertir la dirección de
/// dependencias (config no conoce al frontend).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortColumnKey {
    /// Nombre.
    Name,
    /// Tamaño.
    Size,
    /// Fecha de modificación.
    Mtime,
}

/// `[ui.columns]` resuelto (#108): ids CRUDOS (set abierto — los parsea el
/// frontend, los reporta doctor) + sort validado + overrides por scheme.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnsConfig {
    /// Ids de columna en orden de pintado; `None` = default built-in.
    pub default_columns: Option<Vec<String>>,
    /// Orden global; `None` = histórico (name/asc/dirs-first).
    pub sort: Option<SortChoice>,
    /// Overrides por scheme (la lista REEMPLAZA, jamás mezcla).
    pub schemes: std::collections::BTreeMap<String, SchemeColumns>,
    /// Specs globales por id de columna (#108 7b), ya validados.
    pub specs: std::collections::BTreeMap<String, ColumnSpec>,
}

/// Override de un scheme dentro de [`ColumnsConfig`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SchemeColumns {
    /// Lista de columnas del scheme; `None` = hereda la default.
    pub columns: Option<Vec<String>>,
    /// Orden del scheme; `None` = hereda el global.
    pub sort: Option<SortChoice>,
    /// Specs del scheme por id (#108 7b); al resolver GANAN sobre los
    /// globales.
    pub specs: std::collections::BTreeMap<String, ColumnSpec>,
}

/// Width elegido en un spec (#108 7b), ya validado a `[1, 64]`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WidthChoice {
    /// Ancho de la celda más ancha de la página (techo del frontend).
    Auto,
    /// Fijo en celdas.
    Fixed(u16),
    /// Reparto por peso con suelo.
    Flex {
        /// Suelo en celdas.
        min: u16,
        /// Peso del reparto.
        weight: u16,
    },
}

/// Align elegido en un spec (#108 7b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AlignChoice {
    /// Izquierda.
    Left,
    /// Derecha.
    Right,
}

/// Un `[[ui.columns.spec]]` resuelto (#108 7b): vocabularios YA validados
/// (typo = error de carga, patrón sort); `format` queda como string —
/// si CASA con la columna lo decide el frontend (doctor reporta).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColumnSpec {
    /// Ancho, si el spec lo fija.
    pub width: Option<WidthChoice>,
    /// Alineación, si el spec la fija.
    pub align: Option<AlignChoice>,
    /// Formato (vocabulario global cerrado; encaje por-columna = frontend).
    pub format: Option<String>,
    /// Cabecera propia (texto libre; el frontend la sanea y capa).
    pub header: Option<String>,
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
    /// `[ui] font` (last-wins; None = platform default). Honored from ALL
    /// layers including Project — presentation-only, same reasoning as the
    /// other `[ui]` scalars above.
    pub ui_font: Option<String>,
    /// `[ui] mono_font` (last-wins; None = bundled mono). Honored from ALL
    /// layers including Project — presentation-only, same reasoning as the
    /// other `[ui]` scalars above.
    pub ui_mono_font: Option<String>,
    /// `[ui] font_size`, validated to `[8.0, 32.0]` (invalid value = load
    /// error; None = platform/frontend default). Honored from ALL layers
    /// including Project — presentation-only, same reasoning as the other
    /// `[ui]` scalars above.
    pub ui_font_size: Option<f32>,
    /// `[ui] reduce_motion` (last-wins; None = motion allowed, spec §17 a11y
    /// / GUI phase G2). Honored from ALL layers including Project —
    /// presentation-only, same class as `ui_theme`/`ui_lang` above.
    pub ui_reduce_motion: Option<bool>,
    /// `[ui] confirm_quit`, validated (S2, invalid value = load error, same
    /// pattern as `quick_search`). Honored from ALL layers including
    /// Project — presentation-only, same class as the other `[ui]` scalars
    /// above.
    pub ui_confirm_quit: ConfirmQuit,
    /// `[ui.columns]` (#108, last-wins POR CAMPO; schemes se fusionan por
    /// clave con el último ganando). Presentación-solo: todas las capas.
    pub ui_columns: ColumnsConfig,
    /// `[ui] show_hidden` (#107, last-wins; None = show everything). Honored
    /// from ALL layers including Project — presentation-only, same class as
    /// the other `[ui]` scalars above: hiding dotfiles cannot launch, write,
    /// or redirect anything.
    pub ui_show_hidden: Option<bool>,
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

/// Merges one layer's already-parsed `[daemon]` section (already filtered to
/// non-Project by the caller) into the accumulators (last-present-wins per
/// field, infallible). Extracted out of [`load`] to stay under clippy's
/// line-count cap, same pattern as [`merge_ai_layer`].
fn merge_daemon_layer(
    daemon_mode: &mut Option<DaemonMode>,
    daemon_socket: &mut Option<PathBuf>,
    d: crate::schema::DaemonSection,
) {
    if let Some(m) = d.mode {
        *daemon_mode = Some(m);
    }
    if let Some(sock) = d.socket {
        *daemon_socket = Some(sock);
    }
}

/// Merges one layer's already-parsed `[archive]` section (already filtered
/// to non-Project by the caller) into the accumulators (last-present-wins
/// per field, infallible — every field is a plain scalar copy). Extracted
/// out of [`load`] to stay under clippy's line-count cap, same pattern as
/// [`merge_ai_layer`].
fn merge_archive_layer(
    archive_max_entries: &mut Option<u64>,
    archive_max_decompressed_bytes: &mut Option<u64>,
    archive_max_nesting: &mut Option<usize>,
    a: &crate::schema::ArchiveSection,
) {
    if let Some(n) = a.max_entries {
        *archive_max_entries = Some(n);
    }
    if let Some(b) = a.max_decompressed_bytes {
        *archive_max_decompressed_bytes = Some(b);
    }
    if let Some(n) = a.max_nesting {
        *archive_max_nesting = Some(n);
    }
}

/// Merges one layer's already-parsed `[ui] font`/`mono_font`/`font_size`/
/// `reduce_motion` values into the accumulators (last-present-wins),
/// validating `font_size` against `[8.0, 32.0]` (GP:
/// `docs/superpowers/specs/2026-07-23-gui-visual-plugins-design.md`;
/// `reduce_motion` is G2, spec §17 a11y — no validation needed, any bool is
/// valid). Extracted out of [`load`] to stay under clippy's line-count cap,
/// same pattern as [`merge_ai_layer`].
///
/// # Errors
/// [`ConfigError::Toml`] if `font_size` is outside `[8.0, 32.0]`.
#[allow(clippy::too_many_arguments)]
fn merge_ui_fonts(
    ui_font: &mut Option<String>,
    ui_mono_font: &mut Option<String>,
    ui_font_size: &mut Option<f32>,
    ui_reduce_motion: &mut Option<bool>,
    font: Option<String>,
    mono_font: Option<String>,
    font_size: Option<f32>,
    reduce_motion: Option<bool>,
    norte: &Path,
) -> Result<(), ConfigError> {
    if let Some(f) = font {
        *ui_font = Some(f);
    }
    if let Some(mf) = mono_font {
        *ui_mono_font = Some(mf);
    }
    if let Some(fs) = font_size {
        if !(8.0..=32.0).contains(&fs) {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                // #73: never quote the raw value in the diagnostic.
                message: "[ui] font_size fuera de rango [8, 32]".to_owned(),
            });
        }
        *ui_font_size = Some(fs);
    }
    if let Some(rm) = reduce_motion {
        *ui_reduce_motion = Some(rm);
    }
    Ok(())
}

/// Fusiona una capa de `[ui.columns]` sobre el acumulado (#108): last-wins
/// por campo; los schemes se fusionan por clave (el último gana por campo).
fn merge_ui_columns(
    acc: &mut ColumnsConfig,
    cols: &crate::schema::UiColumnsSection,
    norte: &Path,
) -> Result<(), ConfigError> {
    if let Some(d) = &cols.default {
        acc.default_columns = Some(d.clone());
    }
    if let Some(sort) = &cols.sort {
        acc.sort = Some(parse_sort_section(sort, norte)?);
    }
    if let Some(spec) = &cols.spec {
        for (id, s) in parse_spec_entries(spec, norte, "[ui.columns]")? {
            fold_spec_into(&mut acc.specs, id, s);
        }
    }
    if let Some(schemes) = &cols.scheme {
        for (k, v) in schemes {
            let entry = acc.schemes.entry(k.clone()).or_default();
            if let Some(c) = &v.columns {
                entry.columns = Some(c.clone());
            }
            if let Some(sort) = &v.sort {
                entry.sort = Some(parse_sort_section(sort, norte)?);
            }
            if let Some(spec) = &v.spec {
                for (id, s) in parse_spec_entries(spec, norte, "[ui.columns.scheme]")? {
                    fold_spec_into(&mut entry.specs, id, s);
                }
            }
        }
    }
    Ok(())
}

/// Valida las entradas `[[ui.columns.spec]]` de UNA capa (#108 7b): el `id`
/// es un set abierto (lo parsea el frontend, lo reporta doctor), pero cada
/// vocabulario es CERRADO — un typo es error de carga con la ruta culpable
/// (patrón sort), jamás un skip silencioso. Un `id` repetido dentro de la
/// capa fusiona last-wins POR CAMPO, igual que entre capas (mismo criterio
/// que la hotlist intra-capa). El diagnóstico jamás cita el valor crudo
/// (#73).
fn parse_spec_entries(
    raw: &[schema::ColumnSpecSection],
    norte: &Path,
    label: &str,
) -> Result<std::collections::BTreeMap<String, ColumnSpec>, ConfigError> {
    let mut out = std::collections::BTreeMap::new();
    for entry in raw {
        if entry.id.is_empty() {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                message: format!("{label} spec.id: no puede estar vacío"),
            });
        }
        let width = entry
            .width
            .as_ref()
            .map(|w| parse_spec_width(w, norte, label))
            .transpose()?;
        let align = match entry.align.as_deref() {
            None => None,
            Some("left") => Some(AlignChoice::Left),
            Some("right") => Some(AlignChoice::Right),
            Some(_) => {
                return Err(ConfigError::Toml {
                    path: norte.to_path_buf(),
                    message: format!("{label} spec.align: left | right"),
                });
            }
        };
        let format = match entry.format.as_deref() {
            None => None,
            Some(f @ ("exact" | "iec" | "si" | "relative" | "iso" | "octal" | "rwx")) => {
                Some(f.to_owned())
            }
            Some(_) => {
                return Err(ConfigError::Toml {
                    path: norte.to_path_buf(),
                    message: format!(
                        "{label} spec.format: exact | iec | si | relative | iso | octal | rwx"
                    ),
                });
            }
        };
        fold_spec_into(
            &mut out,
            entry.id.clone(),
            ColumnSpec {
                width,
                align,
                format,
                header: entry.header.clone(),
            },
        );
    }
    Ok(out)
}

/// Valida el `width` de un spec (#108 7b): keyword solo `"auto"`;
/// `fixed`/`min` acotados a `[1, 64]` (0 celdas no pinta nada y >64 se
/// come el pane). `weight` queda libre (0 = no crece, documentado).
fn parse_spec_width(
    raw: &schema::WidthSection,
    norte: &Path,
    label: &str,
) -> Result<WidthChoice, ConfigError> {
    let range_err = || ConfigError::Toml {
        path: norte.to_path_buf(),
        message: format!("{label} spec.width: fixed/min fuera de rango [1, 64]"),
    };
    match raw {
        schema::WidthSection::Keyword(s) if s == "auto" => Ok(WidthChoice::Auto),
        schema::WidthSection::Keyword(_) => Err(ConfigError::Toml {
            path: norte.to_path_buf(),
            message: format!(
                "{label} spec.width: \"auto\" | {{ fixed = n }} | {{ min = n, weight = m }}"
            ),
        }),
        schema::WidthSection::Fixed { fixed } => (1..=64)
            .contains(fixed)
            .then_some(WidthChoice::Fixed(*fixed))
            .ok_or_else(range_err),
        schema::WidthSection::Flex { min, weight } => (1..=64)
            .contains(min)
            .then_some(WidthChoice::Flex {
                min: *min,
                weight: *weight,
            })
            .ok_or_else(range_err),
    }
}

/// Fusiona `spec` sobre `map[id]` last-wins POR CAMPO (`Some` pisa, `None`
/// conserva la capa anterior) — el mismo criterio en la fusión intra-capa
/// y entre capas.
fn fold_spec_into(
    map: &mut std::collections::BTreeMap<String, ColumnSpec>,
    id: String,
    spec: ColumnSpec,
) {
    let e = map.entry(id).or_default();
    if let Some(w) = spec.width {
        e.width = Some(w);
    }
    if let Some(a) = spec.align {
        e.align = Some(a);
    }
    if let Some(f) = spec.format {
        e.format = Some(f);
    }
    if let Some(h) = spec.header {
        e.header = Some(h);
    }
}

/// Valida un [`crate::schema::SortSection`] (#108): vocabulario CERRADO,
/// inválido = error con la ruta (patrón `quick_search`). Los campos
/// ausentes caen al default histórico.
fn parse_sort_section(
    raw: &crate::schema::SortSection,
    norte: &Path,
) -> Result<SortChoice, ConfigError> {
    let column = match raw.column.as_deref() {
        None | Some("name") => SortColumnKey::Name,
        Some("size") => SortColumnKey::Size,
        Some("mtime") => SortColumnKey::Mtime,
        Some(_) => {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                message: "[ui.columns] sort.column: name | size | mtime".to_owned(),
            });
        }
    };
    let descending = match raw.dir.as_deref() {
        None | Some("asc") => false,
        Some("desc") => true,
        Some(_) => {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                message: "[ui.columns] sort.dir: asc | desc".to_owned(),
            });
        }
    };
    Ok(SortChoice {
        column,
        descending,
        dirs_first: raw.dirs_first.unwrap_or(true),
    })
}

/// Parses `[ui] quick_search`'s raw string into [`QuickSearch`]. Same #73
/// caution as `toml_diag`: a hostile TOML could stuff a bidi/kilometric
/// string into anything, and this is a two-value field — naming the
/// offending file plus the two valid values is enough context, no need to
/// reflect `raw` itself. Extracted out of [`load`] to stay under clippy's
/// line-count cap, same pattern as the `merge_*` helpers above.
///
/// # Errors
/// [`ConfigError::Toml`] if `raw` isn't `"filter"`/`"jump"`.
fn parse_quick_search(raw: &str, norte: &Path) -> Result<QuickSearch, ConfigError> {
    match raw {
        "filter" => Ok(QuickSearch::Filter),
        "jump" => Ok(QuickSearch::Jump),
        _ => Err(ConfigError::Toml {
            path: norte.to_path_buf(),
            message: "[ui] quick_search inválido: solo se admite «filter» o «jump»".to_owned(),
        }),
    }
}

/// Parses `[ui] confirm_quit`'s raw string into [`ConfirmQuit`] (S2). Same
/// #73 caution as `quick_search`'s inline match: the diagnostic never quotes
/// the raw value — this field only has three valid values, so naming them is
/// enough context. Extracted out of [`load`] to stay under clippy's
/// line-count cap, same pattern as the `merge_*` helpers above.
///
/// # Errors
/// [`ConfigError::Toml`] if `raw` isn't `"auto"`/`"always"`/`"never"`.
fn parse_confirm_quit(raw: &str, norte: &Path) -> Result<ConfirmQuit, ConfigError> {
    match raw {
        "auto" => Ok(ConfirmQuit::Auto),
        "always" => Ok(ConfirmQuit::Always),
        "never" => Ok(ConfirmQuit::Never),
        _ => Err(ConfigError::Toml {
            path: norte.to_path_buf(),
            message: "[ui] confirm_quit inválido: solo se admite «auto», «always» o «never»"
                .to_owned(),
        }),
    }
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
    let mut ui_font: Option<String> = None;
    let mut ui_mono_font: Option<String> = None;
    let mut ui_font_size: Option<f32> = None;
    let mut ui_reduce_motion: Option<bool> = None;
    let mut ui_confirm_quit = ConfirmQuit::default();
    let mut ui_show_hidden: Option<bool> = None;
    let mut ui_columns = ColumnsConfig::default();
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
            if let Some(qs) = &parsed.ui.quick_search {
                quick_search = parse_quick_search(qs, &norte)?;
            }
            merge_ui_fonts(
                &mut ui_font,
                &mut ui_mono_font,
                &mut ui_font_size,
                &mut ui_reduce_motion,
                parsed.ui.font,
                parsed.ui.mono_font,
                parsed.ui.font_size,
                parsed.ui.reduce_motion,
                &norte,
            )?;
            if let Some(cq) = &parsed.ui.confirm_quit {
                ui_confirm_quit = parse_confirm_quit(cq, &norte)?;
            }
            ui_show_hidden = parsed.ui.show_hidden.or(ui_show_hidden);
            if let Some(cols) = &parsed.ui.columns {
                merge_ui_columns(&mut ui_columns, cols, &norte)?;
            }
            // `[daemon]` is NOT honored from Project either (review MAJOR-1):
            // a foreign repo must not redirect the core transport to an
            // attacker-controlled socket — same fail-closed carve-out as
            // `[archive]`/`[ai]`/hotlist.
            if *kind != Layer::Project {
                merge_daemon_layer(&mut daemon_mode, &mut daemon_socket, parsed.daemon);
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
                merge_archive_layer(
                    &mut archive_max_entries,
                    &mut archive_max_decompressed_bytes,
                    &mut archive_max_nesting,
                    &parsed.archive,
                );
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
        ui_font,
        ui_mono_font,
        ui_font_size,
        ui_reduce_motion,
        ui_confirm_quit,
        ui_show_hidden,
        ui_columns,
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
    fn ui_fonts_se_cargan_y_validan() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nfont = \"Inter\"\nmono_font = \"JetBrains Mono\"\nfont_size = 15.5\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_font.as_deref(), Some("Inter"));
        assert_eq!(cfg.ui_mono_font.as_deref(), Some("JetBrains Mono"));
        assert!((cfg.ui_font_size.unwrap() - 15.5).abs() < f32::EPSILON);
    }

    /// `[ui.columns]` (#108): sort validado (vocabulario cerrado, inválido
    /// = error con ruta), ids crudos last-wins, schemes fusionados por
    /// clave con el último ganando por campo.
    #[test]
    fn ui_columns_carga_valida_y_fusiona() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ui.columns]\ndefault = [\"name\", \"size\"]\nsort = { column = \"mtime\", dir = \"desc\" }\n[ui.columns.scheme.sftp]\ncolumns = [\"name\", \"attr:posix.mode\"]\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui.columns.scheme.sftp]\nsort = { column = \"size\" }\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(&["name".to_owned(), "size".to_owned()][..])
        );
        assert_eq!(
            cfg.ui_columns.sort,
            Some(SortChoice {
                column: SortColumnKey::Mtime,
                descending: true,
                dirs_first: true
            })
        );
        let sftp = &cfg.ui_columns.schemes["sftp"];
        assert_eq!(
            sftp.columns.as_deref(),
            Some(&["name".to_owned(), "attr:posix.mode".to_owned()][..]),
            "la capa user no la pisó (solo trajo sort)"
        );
        assert_eq!(
            sftp.sort,
            Some(SortChoice {
                column: SortColumnKey::Size,
                descending: false,
                dirs_first: true
            })
        );

        // Vocabulario cerrado: columna de sort inválida = error de carga.
        let bad = tempfile::tempdir().unwrap();
        std::fs::write(
            bad.path().join("norte.toml"),
            "[ui.columns]\nsort = { column = \"colour\" }\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(bad.path().to_path_buf(), Layer::User)],
        };
        assert!(load(&layers).is_err());
    }

    /// Carga una única capa User desde un string TOML (harness compacto para
    /// los tests de `[[ui.columns.spec]]`; mismo esqueleto que
    /// `ui_columns_carga_valida_y_fusiona`).
    fn carga_una_capa_result(toml: &str) -> Result<CommonConfig, ConfigError> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), toml).unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        load(&layers)
    }

    fn carga_una_capa(toml: &str) -> CommonConfig {
        carga_una_capa_result(toml).expect("carga")
    }

    /// `[[ui.columns.spec]]` (#108 7b): parse de la capa única — spec global
    /// por id + spec de scheme que convive (la precedencia al RESOLVER es
    /// del frontend; aquí solo se pina que ambos mapas llegan).
    #[test]
    fn ui_columns_spec_carga_valida_y_precedencia_scheme() {
        let toml = r#"
[[ui.columns.spec]]
id = "size"
format = "si"
header = "Peso"
width = { fixed = 9 }

[[ui.columns.spec]]
id = "kind"
align = "left"

[[ui.columns.scheme.sftp.spec]]
id = "size"
format = "exact"
"#;
        let cfg = carga_una_capa(toml);
        let g = cfg.ui_columns.specs.get("size").expect("spec global size");
        assert_eq!(g.format.as_deref(), Some("si"));
        assert_eq!(g.header.as_deref(), Some("Peso"));
        assert_eq!(g.width, Some(WidthChoice::Fixed(9)));
        assert_eq!(
            cfg.ui_columns.specs.get("kind").and_then(|s| s.align),
            Some(AlignChoice::Left)
        );
        let sc = cfg.ui_columns.schemes.get("sftp").expect("scheme");
        assert_eq!(
            sc.specs.get("size").and_then(|s| s.format.as_deref()),
            Some("exact")
        );
    }

    /// Vocabularios CERRADOS del spec (#108 7b): typo/rango = error de carga
    /// (patrón sort), jamás un skip silencioso.
    #[test]
    fn ui_columns_spec_vocabularios_cerrados_fallan_al_cargar() {
        for toml in [
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"sise\"\n",
            "[[ui.columns.spec]]\nid = \"size\"\nalign = \"middle\"\n",
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = \"anchisimo\"\n",
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 0 }\n",
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 200 }\n",
            "[[ui.columns.spec]]\nformat = \"iec\"\n", // sin id
        ] {
            assert!(carga_una_capa_result(toml).is_err(), "debió fallar: {toml}");
        }
    }

    /// Pin de comportamiento serde (#108 7b): `WidthSection` es `untagged`
    /// y serde IGNORA `deny_unknown_fields` dentro de variantes struct de
    /// un enum untagged — un campo extra junto a `fixed` se ignora en
    /// silencio (no es error ni panic). Documentado en el rustdoc de
    /// `schema::WidthSection`; si serde cambia, este test avisa.
    #[test]
    fn width_fixed_con_campo_extra_comportamiento_serde() {
        let cfg = carga_una_capa(
            "[[ui.columns.spec]]\nid = \"size\"\nwidth = { fixed = 9, extra = 1 }\n",
        );
        assert_eq!(
            cfg.ui_columns.specs.get("size").and_then(|s| s.width),
            Some(WidthChoice::Fixed(9)),
            "campo extra ignorado, fixed sobrevive"
        );
    }

    /// Merge de specs entre capas (#108 7b): last-wins POR CAMPO por id —
    /// mismo criterio que el resto de `[ui.columns]`.
    #[test]
    fn ui_columns_spec_merge_por_id_ultimo_gana_por_campo() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"iec\"\nheader = \"A\"\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"si\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        let s = cfg.ui_columns.specs.get("size").expect("spec size");
        assert_eq!(s.format.as_deref(), Some("si"), "capa user gana el campo");
        assert_eq!(
            s.header.as_deref(),
            Some("A"),
            "campo no re-declarado conserva la capa anterior"
        );
    }

    /// `[ui] show_hidden` (#107): last-wins, todas las capas — misma clase
    /// presentación-solo que `reduce_motion`. Ausente = None (el frontend
    /// muestra todo).
    #[test]
    fn ui_show_hidden_carga_last_wins_y_ausente_es_none() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ui]\nshow_hidden = true\n",
        )
        .unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui]\nshow_hidden = false\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_show_hidden, Some(false), "last-wins");

        let empty = tempfile::tempdir().unwrap();
        std::fs::write(empty.path().join("norte.toml"), "").unwrap();
        let layers = Layers {
            dirs: vec![(empty.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_show_hidden, None, "ausente = None (mostrar todo)");
    }

    /// `[ui] reduce_motion` (G2 a11y override, spec §17): last-wins, honored
    /// from EVERY layer including Project — same presentation-only class as
    /// `ui_theme`/`ui_lang`, not the security-sensitive fail-closed carve-out
    /// `[archive]`/`[ai]`/hotlist get.
    #[test]
    fn ui_reduce_motion_carga_last_wins_todas_las_capas() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(
            system.path().join("norte.toml"),
            "[ui]\nreduce_motion = true\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[ui]\nreduce_motion = false\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(
            cfg.ui_reduce_motion,
            Some(false),
            "la capa Project gana (last-wins) y SÍ se honra (presentación, no seguridad)"
        );
    }

    #[test]
    fn ui_reduce_motion_ausente_es_none() {
        let cfg = load(&Layers { dirs: vec![] }).expect("carga");
        assert_eq!(cfg.ui_reduce_motion, None);
    }

    /// S2: `[ui] confirm_quit` acepta los tres valores documentados.
    #[test]
    fn confirm_quit_valores_validos() {
        for (raw, expected) in [
            ("auto", ConfirmQuit::Auto),
            ("always", ConfirmQuit::Always),
            ("never", ConfirmQuit::Never),
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(
                dir.path().join("norte.toml"),
                format!("[ui]\nconfirm_quit = \"{raw}\"\n"),
            )
            .unwrap();
            let layers = Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            };
            let cfg = load(&layers).expect("carga");
            assert_eq!(cfg.ui_confirm_quit, expected, "raw={raw}");
        }
    }

    #[test]
    fn confirm_quit_default_es_auto() {
        let cfg = load(&Layers { dirs: vec![] }).expect("carga");
        assert_eq!(cfg.ui_confirm_quit, ConfirmQuit::Auto);
    }

    #[test]
    fn confirm_quit_valor_invalido_es_error_de_carga() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[ui]\nconfirm_quit = \"a-veces\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let err = load(&layers).expect_err("config rota es error (ADR 0007)");
        assert!(matches!(err, ConfigError::Toml { .. }));
    }

    /// `as_str` round-tripea las tres cadenas de wire.
    #[test]
    fn confirm_quit_as_str() {
        assert_eq!(ConfirmQuit::Auto.as_str(), "auto");
        assert_eq!(ConfirmQuit::Always.as_str(), "always");
        assert_eq!(ConfirmQuit::Never.as_str(), "never");
    }

    /// ADR 0007: config inválida es error de arranque CON fichero culpable —
    /// un `font_size` fuera de [8, 32] no se clampa en silencio (contrato
    /// distinto al de [effects], que es data de tema y clampa).
    #[test]
    fn ui_font_size_fuera_de_rango_es_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui]\nfont_size = 4.0\n").unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        assert!(matches!(load(&layers), Err(ConfigError::Toml { .. })));
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

/// Tests de [`persist_set`] (S2): la forma GENÉRICA detrás de
/// `persist_ui_theme_to` — comprueba lo que ese wrapper no ejercita solo
/// (una sección arbitraria, un fichero nuevo, un valor hostil), MIENTRAS que
/// `hotlist_tests::persist_helpers_no_citan_el_error_crudo_de_toml_edit`
/// arriba es la prueba de comportamiento de que el wrapper sigue siendo
/// idéntico a como era.
#[cfg(test)]
mod persist_set_tests {
    use super::*;

    /// Round trip preservando comentarios ya existentes — mismo criterio que
    /// `hotlist_round_trip_preservando_comentarios`.
    #[test]
    fn persist_set_preserva_comentarios_existentes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# mi config\n[ui]\ntheme = \"nord\" # tema\n",
        )
        .unwrap();
        persist_set(dir.path(), "ui", "lang", toml_edit::Value::from("es")).unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("# mi config"), "comentario del fichero: {s}");
        assert!(s.contains("# tema"), "comentario de la clave: {s}");
        assert!(s.contains("lang = \"es\""), "{s}");
        assert!(s.contains("theme = \"nord\""), "valor previo intacto: {s}");
    }

    /// Fichero AUSENTE: `persist_set` lo crea (y el dir, si tampoco existe).
    #[test]
    fn persist_set_crea_el_fichero_si_no_existe() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("subdir/aun-no-existe");
        let path = persist_set(&dir, "keymap", "preset", toml_edit::Value::from("vim")).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("preset = \"vim\""), "{s}");
    }

    /// La tabla `[section]` nace EXPLÍCITA en un fichero nuevo — no
    /// `section.key = …` en dotted-key implícito, que sería ilegible/no
    /// idiomático para un fichero que el usuario puede editar a mano.
    #[test]
    fn persist_set_crea_la_seccion_explicita() {
        let dir = tempfile::tempdir().unwrap();
        let path = persist_set(dir.path(), "ai", "enabled", toml_edit::Value::from(true)).unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("[ai]"), "sección EXPLÍCITA: {s}");
        assert!(
            !s.contains("ai.enabled"),
            "no debe degradar a dotted-key implícito: {s}"
        );
    }

    /// Pin encoding: un valor string hostil (comilla, salto de línea, una
    /// cabecera TOML embebida y un override bidi) round-tripea escapado y
    /// byte-idéntico — `toml_edit` escapa, jamás inyecta TOML — mismo
    /// criterio que `hotlist_round_trip_name_hostil_byte_identico`.
    #[test]
    fn persist_set_valor_hostil_round_tripea_escapado() {
        let dir = tempfile::tempdir().unwrap();
        let hostile = "fa\"vo\n[[evil]]\u{202E}rito";
        persist_set(dir.path(), "ui", "font", toml_edit::Value::from(hostile)).unwrap();
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("el valor hostil no rompe el TOML");
        assert_eq!(
            cfg.ui_font.as_deref(),
            Some(hostile),
            "valor byte-idéntico tras el round trip"
        );
    }

    /// `[section]` ya existente se REEMPLAZA (misma clave, valor nuevo) —
    /// una sola ocurrencia en el fichero final, no una duplicada.
    #[test]
    fn persist_set_reemplaza_clave_existente() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).unwrap();
        persist_set(
            dir.path(),
            "ui",
            "theme",
            toml_edit::Value::from("gruvbox-dark"),
        )
        .unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s.matches("theme").count(), 1, "una sola clave: {s}");
        assert!(s.contains("gruvbox-dark"), "{s}");
        assert!(!s.contains("\"nord\""), "{s}");
    }

    /// Revisión S, I1: `[section]` existente pero con forma ESCALAR
    /// (`ui = 3`, p. ej. un `norte.toml` editado a mano entre sesiones) es
    /// un `Err(InvalidData)` LIMPIO — antes de este fix, `toml_edit`
    /// indexaba esa entrada y panicaba (`IndexMut` de un `Item::Value` no
    /// tabla devuelve `None` internamente, `.expect()`d por el operador de
    /// índice). Un panic aquí hundiría el hilo de fondo que llama a
    /// `persist_set` (GUI: se lleva el proceso; TUI: `JoinError` silencioso).
    /// El fichero queda INTACTO (el guard es de solo lectura, antes de
    /// cualquier escritura).
    #[test]
    fn persist_set_seccion_escalar_es_err_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = 3\n").unwrap();
        let err = persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord"))
            .expect_err("[ui] escalar debe rechazarse, no panicar");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s, "ui = 3\n", "el fichero no se toca en el camino de error");
    }

    /// Mismo guard, forma array-of-tables (`[[ui]]`) — igual de "no tabla"
    /// para nuestro propósito aunque `toml_edit` lo modele como su propio
    /// tipo (`Item::ArrayOfTables`), no como un `Item::Value` escalar.
    #[test]
    fn persist_set_seccion_array_of_tables_es_err_no_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[[ui]]\nx = 1\n").unwrap();
        let err = persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord"))
            .expect_err("[[ui]] array-of-tables debe rechazarse, no panicar");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
    }

    /// Par positivo del guard: una sección `[section]` inline
    /// (`ui = { theme = "x" }`) SÍ es tabla-like para `toml_edit` — el guard
    /// no debe rechazarla (pin: evita que un guard demasiado estricto rompa
    /// una forma que la librería indexa sin problema).
    #[test]
    fn persist_set_seccion_inline_table_no_se_rechaza() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = { theme = \"nord\" }\n").unwrap();
        persist_set(dir.path(), "ui", "lang", toml_edit::Value::from("es"))
            .expect("tabla inline: el guard no debe rechazarla");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("lang"), "{s}");
    }
}

/// Tests de [`persist_columns`] (#108 7a): el PRIMER valor array que el
/// persistidor escribe jamás — el round trip por el `load` real es el pin
/// del contrato (los nombres de clave del sort son EXACTAMENTE los que
/// parsea `parse_sort_section`: `column`/`dir`/`dirs_first`).
#[cfg(test)]
mod persist_columns_tests {
    use super::*;

    /// Sin scheme → `[ui.columns] default + sort`, y el `load` real lo relee
    /// idéntico (ids opacos incluidos — el picker jamás limpia la config).
    #[test]
    fn persist_columns_default_round_tripea_por_load() {
        let dir = tempfile::tempdir().unwrap();
        persist_columns(
            dir.path(),
            None,
            &[
                "name".to_owned(),
                "mtime".to_owned(),
                "attr:posix.mode".to_owned(),
            ],
            PersistSort {
                column: "mtime",
                descending: true,
                dirs_first: true,
            },
        )
        .expect("escritura");
        // La hoja se emite EXPLÍCITA (un `[ui.columns]` legible), no como
        // dotted-keys implícitos — mismo criterio que `persist_set`.
        let texto = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(texto.contains("[ui.columns]"), "hoja explícita: {texto}");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(
                &[
                    "name".to_owned(),
                    "mtime".to_owned(),
                    "attr:posix.mode".to_owned()
                ][..]
            )
        );
        assert_eq!(
            cfg.ui_columns.sort,
            Some(SortChoice {
                column: SortColumnKey::Mtime,
                descending: true,
                dirs_first: true
            })
        );
    }

    /// Pin M1/L1 (revisión 7a): el PRIMER camino de escritura de ARRAY del
    /// persistidor con un id hostil (comilla + salto de línea + header de
    /// sección + RLO embebidos). `toml_edit` lo escapa (multi-line escapes),
    /// jamás inyecta TOML: el `load` real relee la lista BYTE-IDÉNTICA, el
    /// hostil sigue siendo UN elemento y la config no gana artefactos
    /// (schemes intacto). El pin hostil existente
    /// (`hotlist_round_trip_name_hostil_byte_identico`) solo cubría Values
    /// escalares.
    #[test]
    fn persist_columns_id_hostil_round_tripea_por_load() {
        let dir = tempfile::tempdir().unwrap();
        let hostil = "x\"]\n[evil]\u{202E}";
        persist_columns(
            dir.path(),
            None,
            &["name".to_owned(), hostil.to_owned()],
            PersistSort {
                column: "name",
                descending: false,
                dirs_first: true,
            },
        )
        .expect("escritura");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("el id hostil no rompe el TOML");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(&["name".to_owned(), hostil.to_owned()][..]),
            "la lista round-tripea byte-idéntica, sin inyección"
        );
        assert!(
            cfg.ui_columns.schemes.is_empty(),
            "sin artefactos inyectados: {:?}",
            cfg.ui_columns.schemes
        );
    }

    /// Con scheme → `[ui.columns.scheme.<s>] columns + sort`, preservando
    /// comentarios y lo previo del fichero (`toml_edit`).
    #[test]
    fn persist_columns_scheme_escribe_el_override_y_preserva_comentarios() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# mi config\n[ui]\ntheme = \"default\"\n",
        )
        .expect("seed");
        persist_columns(
            dir.path(),
            Some("sftp"),
            &["name".to_owned(), "size".to_owned()],
            PersistSort {
                column: "size",
                descending: false,
                dirs_first: true,
            },
        )
        .expect("escritura");
        let texto = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(
            texto.contains("# mi config"),
            "comentarios preservados: {texto}"
        );
        assert!(
            texto.contains("theme = \"default\""),
            "lo previo intacto: {texto}"
        );
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        let sc = cfg.ui_columns.schemes.get("sftp").expect("override sftp");
        assert_eq!(
            sc.columns.as_deref(),
            Some(&["name".to_owned(), "size".to_owned()][..])
        );
        assert_eq!(
            sc.sort,
            Some(SortChoice {
                column: SortColumnKey::Size,
                descending: false,
                dirs_first: true
            })
        );
    }

    /// Par positivo del guard (pin del walk por `TableLike`): un `[ui]` en
    /// forma INLINE (`ui = { theme = "nord" }`) pasa `is_table_like` y el
    /// escritor debe ESCRIBIR A TRAVÉS de él — un walk por `as_table_mut`
    /// (solo `Item::Table`) lo rechazaría — sin perder el valor previo.
    #[test]
    fn persist_columns_escribe_a_traves_de_ui_inline() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = { theme = \"nord\" }\n").expect("seed");
        persist_columns(
            dir.path(),
            None,
            &["name".to_owned()],
            PersistSort {
                column: "name",
                descending: false,
                dirs_first: true,
            },
        )
        .expect("tabla inline: el guard no debe rechazarla");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns.default_columns.as_deref(),
            Some(&["name".to_owned()][..])
        );
        assert_eq!(
            cfg.ui_theme.as_deref(),
            Some("nord"),
            "el valor inline previo sobrevive a la escritura"
        );
    }

    /// Guard de forma nivel a nivel (mismo criterio que `persist_set`): un
    /// nivel escalar (`ui = 3`) es `Err(InvalidData)` LIMPIO, no un panic
    /// que tumbaría el hilo de fondo — y el fichero queda intacto.
    #[test]
    fn persist_columns_rechaza_ui_no_tabla_sin_panico() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "ui = 3\n").expect("seed");
        let err = persist_columns(
            dir.path(),
            None,
            &["name".to_owned()],
            PersistSort {
                column: "name",
                descending: false,
                dirs_first: true,
            },
        )
        .expect_err("forma inesperada");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s, "ui = 3\n", "el fichero no se toca en el camino de error");
    }

    /// #108 7b: sin entrada previa, `persist_column_format` crea el
    /// `[[ui.columns.spec]]` con `id` + `format`.
    #[test]
    fn persist_column_format_crea_la_entrada() {
        let dir = tempfile::tempdir().unwrap();
        persist_column_format(dir.path(), "size", "si").expect("escritura");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(
            s.contains("[[ui.columns.spec]]"),
            "AoT bajo [ui.columns]: {s}"
        );
        assert!(s.contains(r#"id = "size""#), "{s}");
        assert!(s.contains(r#"format = "si""#), "{s}");
    }

    /// Reemplazo POR ID: la entrada existente conserva sus OTROS campos
    /// (header) y los comentarios del fichero; jamás nace una segunda
    /// entrada para el mismo id.
    #[test]
    fn persist_column_format_reemplaza_por_id_preservando_campos() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "# mi config\n[[ui.columns.spec]]\nid = \"size\"\nheader = \"Peso\"\nformat = \"iec\"\n",
        )
        .expect("seed");
        persist_column_format(dir.path(), "size", "exact").expect("escritura");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("# mi config"), "comentarios preservados: {s}");
        assert!(
            s.contains(r#"header = "Peso""#),
            "los otros campos sobreviven: {s}"
        );
        assert!(s.contains(r#"format = "exact""#), "{s}");
        assert!(!s.contains(r#"format = "iec""#), "sin entrada vieja: {s}");
        assert_eq!(
            s.matches(r#"id = "size""#).count(),
            1,
            "UNA entrada por id: {s}"
        );
    }

    /// El `load` real relee lo escrito: dos ids → dos specs con su formato.
    #[test]
    fn persist_column_format_round_tripea_por_load() {
        let dir = tempfile::tempdir().unwrap();
        persist_column_format(dir.path(), "size", "si").expect("size");
        persist_column_format(dir.path(), "mtime", "iso").expect("mtime");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns
                .specs
                .get("size")
                .and_then(|sp| sp.format.as_deref()),
            Some("si")
        );
        assert_eq!(
            cfg.ui_columns
                .specs
                .get("mtime")
                .and_then(|sp| sp.format.as_deref()),
            Some("iso")
        );
    }

    /// MAJOR revisión 7b: con DOS entradas del mismo id editadas a mano, el
    /// loader honra la ÚLTIMA (merge intra-capa last-wins por campo) — el
    /// writer debe actualizarlas TODAS o el reload revierte lo recién
    /// guardado. Tras persistir, ambas llevan el formato nuevo y el `load`
    /// real devuelve el valor persistido.
    #[test]
    fn persist_column_format_actualiza_todos_los_duplicados() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("norte.toml"),
            "[[ui.columns.spec]]\nid = \"size\"\nformat = \"iec\"\n\n\
             [[ui.columns.spec]]\nid = \"size\"\nformat = \"exact\"\n",
        )
        .expect("seed");
        persist_column_format(dir.path(), "size", "si").expect("escritura");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(
            s.matches(r#"format = "si""#).count(),
            2,
            "TODOS los duplicados llevan el formato nuevo: {s}"
        );
        assert!(!s.contains(r#"format = "iec""#), "{s}");
        assert!(!s.contains(r#"format = "exact""#), "{s}");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(
            cfg.ui_columns
                .specs
                .get("size")
                .and_then(|sp| sp.format.as_deref()),
            Some("si"),
            "el reload devuelve lo persistido, no el duplicado rancio"
        );
    }

    /// Guard de forma del precedente hotlist: `spec` escalar = error
    /// limpio, sin pánico y sin tocar el fichero.
    #[test]
    fn persist_column_format_rechaza_spec_no_array_sin_panico() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("norte.toml"), "[ui.columns]\nspec = 3\n").expect("seed");
        let err = persist_column_format(dir.path(), "size", "si").expect_err("forma inesperada");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(
            s, "[ui.columns]\nspec = 3\n",
            "el fichero no se toca en el camino de error"
        );
    }
}

#[cfg(test)]
mod persist_atomicity_tests {
    use super::*;

    /// #116: los escritores toman el lock advisory cross-process
    /// (`norte.toml.lock`) ANTES de leer. Con el lock en manos de "otro
    /// proceso" (otro descriptor — mismo mecanismo `flock`/`LockFileEx`),
    /// `persist_set` BLOQUEA hasta la liberación; sin lock, dos RMW se
    /// intercalan y el segundo escribe sobre una lectura rancia (lost
    /// update).
    #[test]
    fn persist_set_espera_el_lock_de_otro_escritor() {
        let dir = tempfile::tempdir().unwrap();
        // Mismo open SIN truncar que `lock_user_toml` (review MAJOR-1).
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("norte.toml.lock"))
            .unwrap();
        holder.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let d = dir.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            let r = persist_set(&d, "ui", "theme", toml_edit::Value::from("nord"));
            let _ = tx.send(());
            r
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "persist_set NO debe completar mientras otro escritor tiene el lock"
        );
        // Cinturón (review MINOR-6): además de no completar, no ha ESCRITO —
        // el lock se toma antes de leer, así que ni el tmp ni el fichero
        // final pueden existir aún.
        assert!(
            !dir.path().join("norte.toml").exists(),
            "nada escrito mientras el lock está en manos ajenas"
        );
        drop(holder); // flock/LockFileEx se libera al cerrar el descriptor
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("liberado el lock, el escritor completa");
        writer.join().unwrap().expect("escritura");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert!(s.contains("theme"), "la escritura aterrizó tras el lock");
    }

    /// #116: dos escritores RMW concurrentes sobre claves distintas no se
    /// pisan — ambas claves sobreviven con su último valor en el fichero
    /// final (sin lock, una lectura rancia descarta la clave del otro).
    #[test]
    fn escritores_concurrentes_no_pierden_claves() {
        let dir = tempfile::tempdir().unwrap();
        let d1 = dir.path().to_path_buf();
        let d2 = dir.path().to_path_buf();
        let a = std::thread::spawn(move || {
            for i in 0..25 {
                persist_set(&d1, "ui", "alpha", toml_edit::Value::from(i)).expect("a");
            }
        });
        let b = std::thread::spawn(move || {
            for i in 0..25 {
                persist_set(&d2, "ui", "beta", toml_edit::Value::from(i)).expect("b");
            }
        });
        a.join().unwrap();
        b.join().unwrap();
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        let doc: toml_edit::DocumentMut = s.parse().expect("el fichero final parsea");
        let ui = doc["ui"].as_table_like().expect("[ui] presente");
        assert_eq!(
            ui.get("alpha").and_then(toml_edit::Item::as_integer),
            Some(24),
            "la última escritura de `alpha` sobrevive"
        );
        assert_eq!(
            ui.get("beta").and_then(toml_edit::Item::as_integer),
            Some(24),
            "la última escritura de `beta` sobrevive"
        );
    }

    /// #116 (pin): la escritura es tmp hermano + rename — tras persistir no
    /// queda temporal residual en el dir (un crash a medias deja como mucho
    /// un tmp huérfano que la siguiente escritura pisa; jamás un
    /// `norte.toml` truncado).
    #[test]
    fn persistir_no_deja_tmp_residual() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).expect("escritura");
        persist_hotlist_add(dir.path(), "docs", "file:///docs").expect("hotlist");
        let residuales: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(residuales.is_empty(), "tmp residual: {residuales:?}");
    }
}
