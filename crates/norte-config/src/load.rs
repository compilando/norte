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

/// The scalar config layer: the file every `persist_*` helper below writes.
const NORTE_TOML: &str = "norte.toml";

/// The keymap layer (ADR 0006/0007) — a SECOND writable config file since
/// K3c ([`persist_keymap_append`]/[`persist_keymap_remove`]). It has its own
/// lock and its own tmp sibling; see [`ConfigFileLock`] for why sharing
/// `norte.toml`'s would be a bug rather than a saving.
const KEYMAP_TOML: &str = "keymap.toml";

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
/// persiste — ver `lock_config_file`) y reemplaza el fichero vía tmp +
/// `rename` atómico (`write_config_file`): un lector concurrente jamás ve
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
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
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
    write_config_file(&lock, &doc)?;
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
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = open_config_toml(&path)?;
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
    write_config_file(&lock, &doc)?;
    Ok(path)
}

/// Lock advisory cross-process de UN fichero de config (#116): `flock`/
/// `LockFileEx` sobre el hermano DEDICADO `<fichero>.lock` — jamás sobre el
/// fichero mismo: la escritura atómica lo reemplaza por `rename` (inode
/// nuevo) y un lock sobre el inode viejo no excluiría al siguiente
/// escritor. Los escritores lo toman ANTES de leer: la sección crítica es
/// el ciclo lee-modifica-escribe ENTERO (lost update cerrado, también
/// entre procesos — GUI + TUI sobre el mismo fichero). Se libera al soltar
/// el guard (cerrar el descriptor); el SO lo suelta igualmente si el
/// proceso muere — no hay locks rancios tras un crash.
///
/// K3c: hay DOS ficheros escribibles (`norte.toml` y `keymap.toml`) y cada
/// uno tiene su PROPIO lock — compartir uno serializaría dos ficheros que no
/// se tocan y, mucho peor, dejaría a un escritor futuro reemplazar un fichero
/// mientras sostiene el lock del OTRO, creyéndose protegido. Que eso sea
/// imposible es estructural, no una convención: el guard lleva su
/// [`ConfigFileLock::target`] y [`write_config_file`] toma de ahí la ruta que
/// escribe, así que el único fichero que un escritor puede nombrar es el que
/// bloqueó.
struct ConfigFileLock {
    /// Mantiene vivo el descriptor bloqueado; drop = cerrar = unlock.
    _file: std::fs::File,
    /// El fichero de config que este lock protege (`dir/<fichero>`), NO el
    /// `.lock` hermano.
    target: PathBuf,
}

impl ConfigFileLock {
    /// El fichero de config protegido — el ÚNICO que su portador puede
    /// escribir (ver la doc del tipo).
    fn target(&self) -> &Path {
        &self.target
    }
}

/// Toma (bloqueando) el lock de escritores de `file` dentro de `dir`.
/// BLOQUEANTE como el resto del persistidor (regla 2: el caller ya envuelve
/// en `spawn_blocking`); los escritores son cortos — retener el lock
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
///
/// `file` es SIEMPRE una constante de este módulo ([`NORTE_TOML`],
/// [`KEYMAP_TOML`]) — nunca un nombre que venga del usuario.
fn lock_config_file(dir: &Path, file: &str) -> std::io::Result<ConfigFileLock> {
    let handle = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join(format!("{file}.lock")))?;
    handle.lock()?;
    Ok(ConfigFileLock {
        _file: handle,
        target: dir.join(file),
    })
}

/// Escritura ATÓMICA del fichero que `lock` protege (#116): tmp hermano +
/// `rename` (atómico en POSIX; `std::fs::rename` reemplaza también en
/// Windows). Un lector concurrente ve el fichero viejo o el nuevo COMPLETO —
/// jamás un truncado a medias que parsee "bien" y del que un escritor
/// posterior reconstruya el documento perdiendo secciones ajenas. `sync_all`
/// antes del rename evita la ventana fichero-vacío-tras-crash; el rename
/// mismo puede perderse en un corte de luz (sin fsync del dir, a propósito):
/// reaparece la config VIEJA — consistente, solo rancia. Un tmp huérfano
/// de un crash es inocuo: la siguiente escritura (mismo nombre, bajo el
/// lock) lo pisa. Los permisos del fichero existente se COPIAN al tmp
/// (review #116 MINOR-2: sin esto un `chmod 600` del usuario se ensanchaba
/// al umask en el reemplazo). Limitación Windows conocida: un proceso
/// externo (editor, AV) con el fichero abierto sin `FILE_SHARE_DELETE`
/// hace fallar el rename con sharing violation — la persistencia falla
/// visible, sin retry (los lectores de Rust std comparten en modo full).
///
/// K3c: la ruta viene del `lock` (no del caller) y el tmp se DERIVA del
/// nombre de esa ruta. Hardcodear `norte.toml.tmp` era inocuo con un solo
/// fichero escribible; con dos, dos escritores de ficheros distintos
/// competirían por un mismo tmp y el rename aterrizaría con el contenido del
/// otro. El nombre se compone en `OsString` (regla 1: los nombres son bytes,
/// jamás se asume UTF-8).
fn write_config_file(lock: &ConfigFileLock, doc: &toml_edit::DocumentMut) -> std::io::Result<()> {
    use std::io::Write;
    let path = lock.target();
    let mut tmp_name = path
        .file_name()
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "ruta de config sin nombre de fichero",
            )
        })?
        .to_os_string();
    tmp_name.push(".tmp");
    let tmp = path.with_file_name(tmp_name);
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

/// Lee (o crea, si no existe) el fichero de config de `path` como documento
/// `toml_edit`, con el error de parseo SANEADO — el `Display` de
/// `toml_edit` cita la línea ofensora, y un `name`/`path` hostil
/// persistido antes llegaría a quien muestre este `io::Error` (la status
/// bar, #73): se nombra el fichero, jamás el contenido. Compartido por los
/// escritores de columnas (`persist_columns`/`persist_column_format`) y por
/// los de keymap (`persist_keymap_append`/`persist_keymap_remove`).
fn open_config_toml(path: &std::path::Path) -> std::io::Result<toml_edit::DocumentMut> {
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
    persist_column_spec_field(dir, id, "format", toml_edit::value(format))
}

/// Persiste el ANCHO de una columna como `width = <celdas>` en su
/// `[[ui.columns.spec]]` (spec 2026-09-11, V2: arrastrar el borde de una
/// cabecera en la ventana). Misma mecánica y mismos guards que
/// [`persist_column_format`]: reemplazo por id, los otros campos y los
/// comentarios sobreviven, todas las entradas duplicadas se actualizan.
///
/// CONTRATO: `cells` ya está en `[1, 64]` —lo que el loader acepta— o el
/// siguiente `load` lo rechazará entero; el caller (el host) acota antes.
/// BLOQUEANTE: I/O de FS síncrono — envolver en `spawn_blocking` (regla 2).
///
/// # Errors
/// Los de [`persist_column_format`].
pub fn persist_column_width(dir: &Path, id: &str, cells: u16) -> std::io::Result<PathBuf> {
    // La forma que el loader lee para un ancho fijo: `width = { fixed = N }`
    // (`WidthSection::Fixed`); un entero a secas no es ninguna variante.
    let mut fijo = toml_edit::InlineTable::new();
    fijo.insert("fixed", toml_edit::Value::from(i64::from(cells)));
    persist_column_spec_field(dir, id, "width", toml_edit::value(fijo))
}

/// El escritor común de UN campo de un `[[ui.columns.spec]]` por id.
fn persist_column_spec_field(
    dir: &Path,
    id: &str,
    key: &str,
    value: toml_edit::Item,
) -> std::io::Result<PathBuf> {
    use std::io::{Error, ErrorKind};
    std::fs::create_dir_all(dir)?;
    // #116: lock ANTES de leer — el RMW entero es la sección crítica.
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = open_config_toml(&path)?;
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
        tb[key] = value.clone();
        alguna = true;
    }
    if !alguna {
        let mut tb = toml_edit::Table::new();
        tb["id"] = toml_edit::value(id);
        tb[key] = value;
        arr.push(tb);
    }
    write_config_file(&lock, &doc)?;
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
    let lock = lock_config_file(dir, NORTE_TOML)?;
    let path = lock.target().to_path_buf();
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
    write_config_file(&lock, &doc)?;
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
    // #116: lock ANTES de leer. Este writer no crea el dir (solo borra):
    // dir inexistente = nada que borrar = el mismo no-op documentado que
    // el `norte.toml` ausente de abajo — y ese no-op necesita la ruta ANTES
    // de que exista el guard, único motivo de que aquí se componga a mano
    // (el resto de la familia la toma de `lock.target()`, que no puede
    // divergir del fichero bloqueado).
    let lock = match lock_config_file(dir, NORTE_TOML) {
        Ok(l) => l,
        Err(e) if e.kind() == ErrorKind::NotFound => return Ok(dir.join(NORTE_TOML)),
        Err(e) => return Err(e),
    };
    let path = lock.target().to_path_buf();
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
            write_config_file(&lock, &doc)?;
        }
    }
    Ok(path)
}

/// The keymap CONTEXTS a binding can be written into (ADR 0006): the four
/// sections of `keymap.toml`, and a CLOSED vocabulary. Mirror of
/// `KeymapFile`'s fields in `norte-frontend` (`keymap/layer.rs`), which is
/// `deny_unknown_fields`: a section this writer invented would not be
/// ignored, it would make the WHOLE layer fail to parse — and a layer that
/// fails to parse reverts the user's entire keymap on the next reload, with
/// the editor having reported success. `norte-config` cannot call that parser
/// (the dependency runs frontend → config, never back), so the vocabulary is
/// mirrored here and pinned from the other side by
/// `norte_frontend::config::tests::un_binding_persistido_carga_y_resuelve`.
const KEYMAP_SECTIONS: [&str; 4] = ["global", "pane", "viewer", "dialog"];

/// Which of a user layer's two binding lists a write goes into (ADR 0006).
///
/// NOT interchangeable, and choosing the wrong one fails SILENTLY. The merge
/// order is: every layer's `prepend_keymap`, then the preset's `keymap`, then
/// every layer's `append_keymap` (`merge_ctx`, `norte-frontend`
/// `keymap/layer.rs`), and the FIRST binding of a sequence wins
/// (`Effective::build_for`). So a chord the preset already binds IN THE SAME
/// context is overridden only by a [`KeymapList::Prepend`]: an append for it
/// parses, loads, validates — and never fires, while the editor reports
/// success. ADR 0006 states the rule for what an append DOES win: "a user
/// append in `pane` overrides a preset binding in `global`" — across
/// contexts, not within one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeymapList {
    /// Wins over the preset: what a REBIND has to use.
    Prepend,
    /// Loses to the preset: for a chord the preset leaves free, where the
    /// binding says "also this" rather than "mine instead".
    Append,
}

impl KeymapList {
    /// The TOML key of this list — the only two a user layer may declare
    /// (`check_layer_keys` refuses the preset's `keymap` in a layer).
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Prepend => "prepend_keymap",
            Self::Append => "append_keymap",
        }
    }

    /// Both lists, in merge order — what [`persist_keymap_unbind`] walks.
    const BOTH: [Self; 2] = [Self::Prepend, Self::Append];
}

/// What a `keymap.toml` write did (K3c).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeymapWrite {
    /// The `keymap.toml` that was written, or would have been.
    pub path: PathBuf,
    /// Whether the file's bytes actually changed. `false` means NOTHING was
    /// written: a bind whose chord already ran that command, or an unbind that
    /// matched nothing. The file keeps its bytes AND its mtime, so the config
    /// watcher does not see a phantom edit and reload for nothing (the rule
    /// [`persist_hotlist_remove`] already documents), and a double confirm in
    /// the editor cannot write the same binding twice.
    pub changed: bool,
}

/// Refuses a binding whose section/chords/command could not make a legal
/// `keymap.toml` entry, BEFORE anything is opened or locked.
///
/// The diagnostics never quote `section`, a chord or `command` (#73): all
/// three are caller data, and this error travels to a status bar.
///
/// CONTRACT — read this before wiring a UI onto these writers. What is
/// rejected here is only what is invalid under ANY chord grammar: an empty
/// sequence, an empty token, an empty command. The chord grammar itself
/// (`keymap::parse_chord`), the command catalogue, ADR 0006's prefix-free
/// rule, the sacred keys and the digit-under-`counts` rule ALL live in
/// `norte-frontend`, which is ABOVE this crate — `norte-config` cannot call
/// them, and a binding that breaks any of them makes `Effective::build_for`
/// fail after the file has loaded perfectly. The caller must run
/// `rebind_check` (K3c c2) against the merged map first; these functions
/// guard the FILE, not the keymap.
fn check_keymap_binding(section: &str, chords: &[String], command: &str) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    if !KEYMAP_SECTIONS.contains(&section) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            format!(
                "unknown keymap section; the contexts are: {}",
                KEYMAP_SECTIONS.join(", ")
            ),
        ));
    }
    if chords.is_empty() || chords.iter().any(String::is_empty) {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "a binding needs at least one non-empty chord",
        ));
    }
    if command.is_empty() {
        return Err(Error::new(
            ErrorKind::InvalidInput,
            "a binding needs a command to run",
        ));
    }
    Ok(())
}

/// The bindings of a binding list, in EITHER shape TOML (and therefore serde)
/// admits: the inline-table array the bundled presets use
/// (`append_keymap = [ { on = […], run = "…" } ]`) and the array of tables a
/// hand-written file may well use (`[[pane.append_keymap]]`). A writer that
/// only knew the first would refuse a file it should extend — or, worse, miss
/// the binding that is already there and write a duplicate.
///
/// `None` = not a list of bindings at all (a scalar, or an array with a
/// non-table element): the caller turns that into a clean shape error instead
/// of writing into something the loader will reject.
fn binding_list(item: &toml_edit::Item) -> Option<Vec<&dyn toml_edit::TableLike>> {
    if let Some(arr) = item.as_array() {
        let mut out: Vec<&dyn toml_edit::TableLike> = Vec::with_capacity(arr.len());
        for v in arr {
            out.push(v.as_inline_table()?);
        }
        return Some(out);
    }
    if let Some(aot) = item.as_array_of_tables() {
        return Some(aot.iter().map(|t| t as &dyn toml_edit::TableLike).collect());
    }
    None
}

/// Is this entry's `on` EXACTLY `chords`? Byte-exact, with no normalisation of
/// any kind: a chord token is a wire vocabulary and two tokens that differ by
/// a byte are two chords — a rebind must never silently replace an entry the
/// user wrote differently (NFC/NFD twins included, the same rule the hotlist
/// keys follow).
fn chord_seq_is(t: &dyn toml_edit::TableLike, chords: &[String]) -> bool {
    t.get("on")
        .and_then(toml_edit::Item::as_array)
        .is_some_and(|on| {
            on.len() == chords.len()
                && on
                    .iter()
                    .zip(chords)
                    .all(|(v, c)| v.as_str() == Some(c.as_str()))
        })
}

/// Does this entry bind `chords` to `command`? Both fields, byte-exact (see
/// [`chord_seq_is`]).
fn binding_is(t: &dyn toml_edit::TableLike, chords: &[String], command: &str) -> bool {
    t.get("run").and_then(toml_edit::Item::as_str) == Some(command) && chord_seq_is(t, chords)
}

/// The shape error for a binding list that is not one. `section` and the list
/// key are both from closed vocabularies when this is reached, so the message
/// quotes only our own words, never caller data (#73).
fn bad_binding_list(path: &Path, section: &str, list: KeymapList) -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::InvalidData,
        format!(
            "{}: [{section}] {} is not a list of bindings (unexpected shape); fix it or delete it",
            path.display(),
            list.key()
        ),
    )
}

/// Refuses a `keymap.toml` that is not a legal USER LAYER before writing into
/// it. `counts = true`, `dialog_from` and a non-empty `keymap` list are
/// PRESET-only, and `check_layer_keys` (`norte-frontend`, `keymap/layer.rs`)
/// makes each of them a LOAD error — which costs the user their whole keymap.
/// This writer can never produce one, but it must not extend a file that
/// already carries one either: the new binding would land in a file that
/// cannot load and the editor would have said "saved".
///
/// Each rule mirrors the loader's EXACTLY, value and all — `counts = false`
/// and `keymap = []` are legal there, so they are legal here. A writer
/// stricter than the loader refuses to save into a file the app itself
/// accepted, and blames the user for it.
///
/// Deliberately NOT a re-implementation of the whole grammar: an unknown key
/// elsewhere in the file also breaks the load (`deny_unknown_fields`), but
/// refusing it here would buy nothing — the file was already broken and this
/// write cannot make it worse — at the price of a second copy of the schema
/// that would rot. What is checked is what this writer is ADJACENT to: the
/// keys that live in, or next to, the sections it writes into.
fn check_user_layer_shape(doc: &toml_edit::DocumentMut, path: &Path) -> std::io::Result<()> {
    use std::io::{Error, ErrorKind};
    let refuse = |key: &str| {
        Error::new(
            ErrorKind::InvalidData,
            format!(
                "{}: `{key}` is a PRESET key, not legal in a user layer (ADR 0006); fix it or delete it",
                path.display()
            ),
        )
    };
    // The loader refuses `if layer.counts` — the VALUE, not the key. An
    // explicit `counts = false` is a legal layer that loads today.
    if doc
        .as_table()
        .get("counts")
        .is_some_and(|it| it.as_bool() != Some(false))
    {
        return Err(refuse("counts"));
    }
    // TOML has no null, so presence means `Some(..)`: the same thing
    // `check_layer_keys` refuses.
    if doc.as_table().contains_key("dialog_from") {
        return Err(refuse("dialog_from"));
    }
    for section in KEYMAP_SECTIONS {
        let Some(full) = doc
            .as_table()
            .get(section)
            .and_then(toml_edit::Item::as_table_like)
            .and_then(|t| t.get("keymap"))
        else {
            continue;
        };
        match binding_list(full) {
            // `check_layer_keys` refuses a NON-EMPTY `keymap` only.
            Some(l) if l.is_empty() => {}
            Some(_) => return Err(refuse("keymap")),
            // Not a list at all: it cannot load either, but calling it a
            // preset key would send the reader to the wrong ADR.
            None => {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!(
                        "{}: [{section}] keymap is not a list of bindings (unexpected shape); fix it or delete it",
                        path.display()
                    ),
                ));
            }
        }
    }
    Ok(())
}

/// Binds `chords` to `command` in `[<section>] <list>` of the `keymap.toml` of
/// `dir` (K3c — the shortcut editor's writer), creating the directory, the
/// file, the section and the list as needed, PRESERVING comments and
/// formatting (`toml_edit`) like the rest of the `persist_*` family.
/// `section` is one of `global`/`pane`/`viewer`/`dialog`; `list` decides
/// whether the binding wins over the preset or loses to it — read
/// [`KeymapList`], the difference is silent.
///
/// INSERT-OR-REPLACE, by chord sequence: an entry in that list already bound
/// to `chords` has its `run` REPLACED in place, keeping its position and the
/// comment beside it. Appending instead would leave two entries for one chord
/// with the OLDER one winning (first wins, in file order), so the second
/// rebind of a key would do nothing — the same silent failure as writing into
/// the wrong list. A later duplicate of the same chord is left alone: it was
/// already inert, and deleting it would take the comment TOML attaches to the
/// element after it.
///
/// IDEMPOTENT: if that entry already runs `command`, nothing is written and
/// [`KeymapWrite::changed`] is `false` — see the field's doc for why an
/// identical rewrite would not be equivalent.
///
/// The two lists are the ONLY ones a user layer may declare; a `keymap` list
/// there is a load error, which is also why a file already carrying a
/// preset-only key is refused rather than extended (`check_user_layer_shape`).
/// This does NOT make every write safe to load: see the CONTRACT on
/// `check_keymap_binding` — the chord grammar, the command catalogue and
/// ADR 0006's whole-map rules live above this crate and are the caller's to
/// check with `rebind_check` (K3c c2) BEFORE calling.
///
/// Cross-process safe like the rest of the family, and with its OWN lock:
/// `keymap.toml.lock`, never `norte.toml.lock` — the private `ConfigFileLock`
/// carries its own target, so writing a file whose lock is not held is
/// unrepresentable.
/// BLOCKING: synchronous FS I/O — the caller MUST wrap it in
/// `spawn_blocking` (rule 2), the same as `persist_hotlist_add`.
///
/// # Errors
/// [`std::io::Error`] if `section` is not a keymap context or the binding is
/// empty ([`std::io::ErrorKind::InvalidInput`]); if the existing TOML does
/// not parse, the section is not a table, the list is not a list of bindings,
/// or the file carries a preset-only key
/// ([`std::io::ErrorKind::InvalidData`]); or if the I/O fails.
pub fn persist_keymap_bind(
    dir: &Path,
    section: &str,
    list: KeymapList,
    chords: &[String],
    command: &str,
) -> std::io::Result<KeymapWrite> {
    check_keymap_binding(section, chords, command)?;
    std::fs::create_dir_all(dir)?;
    // #116: lock BEFORE reading — the whole RMW is the critical section.
    let lock = lock_config_file(dir, KEYMAP_TOML)?;
    let path = lock.target().to_path_buf();
    let mut doc = open_config_toml(&path)?;
    check_user_layer_shape(&doc, &path)?;
    // Read-only pass FIRST: it validates the list's shape before anything is
    // mutated, and it decides idempotence before anything is CREATED — a
    // repeated bind must not materialise an empty section on its way to
    // "nothing changed" and touch the mtime for it.
    if let Some(item) = doc
        .as_table()
        .get(section)
        .and_then(toml_edit::Item::as_table_like)
        .and_then(|t| t.get(list.key()))
    {
        let entries = binding_list(item).ok_or_else(|| bad_binding_list(&path, section, list))?;
        if entries
            .iter()
            .find(|t| chord_seq_is(**t, chords))
            .is_some_and(|t| binding_is(*t, chords, command))
        {
            return Ok(KeymapWrite {
                path,
                changed: false,
            });
        }
    }
    let table = nested_table_mut(&mut doc, &path, &[section])?;
    let item = table.entry(list.key()).or_insert_with(|| {
        let mut fresh = toml_edit::Array::new();
        // A list born here reads like the bundled presets: one binding per
        // line, trailing comma, so the next hand edit has nothing to reflow.
        fresh.set_trailing("\n");
        fresh.set_trailing_comma(true);
        toml_edit::Item::Value(toml_edit::Value::Array(fresh))
    });
    bind_in_list(item, chords, command).ok_or_else(|| bad_binding_list(&path, section, list))?;
    write_config_file(&lock, &doc)?;
    Ok(KeymapWrite {
        path,
        changed: true,
    })
}

/// Replaces the `run` of the FIRST entry bound to `chords`, or pushes a new
/// entry, in whichever of the two legal shapes the list already has (see
/// [`binding_list`]). `None` = not a binding list; the caller has already
/// validated that, so it is the belt to that braces (rule 6: a clean `Err`
/// rather than an `unwrap` on "cannot happen").
///
/// Chords and command go in TAL CUAL: `toml_edit` escapes, it never injects
/// TOML (the pin lives in `persist_set`'s hostile round-trip test).
fn bind_in_list(item: &mut toml_edit::Item, chords: &[String], command: &str) -> Option<()> {
    let mut on = toml_edit::Array::new();
    for c in chords {
        on.push(c.as_str());
    }
    if let Some(arr) = item.as_array_mut() {
        for v in arr.iter_mut() {
            let existing = v.as_inline_table_mut()?;
            if chord_seq_is(existing, chords) {
                existing.insert("run", command.into());
                return Some(());
            }
        }
        let mut inline = toml_edit::InlineTable::new();
        inline.insert("on", toml_edit::Value::Array(on));
        inline.insert("run", command.into());
        // The array's `trailing` is everything between the last comma and the
        // `]`, INCLUDING a trailing comment on the last binding. Pushing after
        // it would hand the user's comment to the new binding, so the comment
        // travels as the new element's prefix — it stays on the line of the
        // binding it annotates.
        let carried = arr
            .trailing()
            .as_str()
            .filter(|t| !t.trim().is_empty())
            .map(str::to_owned);
        let prefix = match &carried {
            Some(t) => format!("{t}    "),
            None => "\n    ".to_owned(),
        };
        if carried.is_some() {
            arr.set_trailing("\n");
        }
        // `push_formatted`, not `push`: `push` applies default formatting and
        // would drop the prefix, packing a growing list onto one unreadable
        // line and dropping the carried comment with it.
        arr.push_formatted(toml_edit::Value::InlineTable(inline).decorated(prefix, ""));
        return Some(());
    }
    if let Some(aot) = item.as_array_of_tables_mut() {
        for existing in aot.iter_mut() {
            if chord_seq_is(existing, chords) {
                existing["run"] = toml_edit::value(command);
                return Some(());
            }
        }
        let mut tb = toml_edit::Table::new();
        tb["on"] = toml_edit::Item::Value(toml_edit::Value::Array(on));
        tb["run"] = toml_edit::value(command);
        aot.push(tb);
        return Some(());
    }
    None
}

/// Removes the binding `chords` → `command` from BOTH of `[<section>]`'s user
/// lists in the `keymap.toml` of `dir` (K3c) — the inverse of
/// [`persist_keymap_bind`], and the reason the editor can fix a mistake
/// instead of only making them. Matches both fields byte-exactly, so it only
/// ever deletes the row the editor showed; every copy of it goes, in both
/// lists, because leaving one behind would leave the key firing after the
/// editor said it was unbound.
///
/// A binding that is not there — or a missing `keymap.toml`, section or list —
/// is a documented NO-OP: [`KeymapWrite::changed`] is `false` and the file is
/// not rewritten (an identical rewrite would still move the mtime and wake the
/// watcher). An emptied value array stays as `<list> = []` rather than being
/// deleted, so a comment attached to the key survives; an emptied array of
/// tables has no such carrier and disappears with its last `[[…]]` header.
/// Note that removing an entry can take a comment written between it and the
/// PREVIOUS binding with it: TOML attaches such a comment to the element that
/// follows it. Nothing else in the file is touched.
///
/// Unlike the bind, a layer carrying a preset-only key is NOT refused here: a
/// removal cannot introduce an illegal shape, and refusing would leave a user
/// whose file has a stray `counts` unable to undo anything through the editor.
///
/// BLOCKING: synchronous FS I/O — the caller MUST wrap it in
/// `spawn_blocking` (rule 2).
///
/// # Errors
/// [`std::io::Error`] if `section` is not a keymap context or the binding is
/// empty ([`std::io::ErrorKind::InvalidInput`]), if the existing TOML does
/// not parse ([`std::io::ErrorKind::InvalidData`]), or if the I/O fails.
pub fn persist_keymap_unbind(
    dir: &Path,
    section: &str,
    chords: &[String],
    command: &str,
) -> std::io::Result<KeymapWrite> {
    check_keymap_binding(section, chords, command)?;
    // No `create_dir_all`: a removal creates nothing. A missing dir is the
    // same documented no-op as a binding that is not there.
    let declared = dir.join(KEYMAP_TOML);
    let lock = match lock_config_file(dir, KEYMAP_TOML) {
        Ok(l) => l,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return Ok(KeymapWrite {
                path: declared,
                changed: false,
            });
        }
        Err(e) => return Err(e),
    };
    let path = lock.target().to_path_buf();
    // A missing file parses as an empty document here, which matches nothing
    // and therefore writes nothing — the no-op above, reached by walking.
    let mut doc = open_config_toml(&path)?;
    let mut changed = false;
    for list in KeymapList::BOTH {
        let Some(item) = doc
            .as_table_mut()
            .get_mut(section)
            .and_then(toml_edit::Item::as_table_like_mut)
            .and_then(|t| t.get_mut(list.key()))
        else {
            continue;
        };
        if let Some(arr) = item.as_array_mut() {
            let before = arr.len();
            arr.retain(|v| {
                !v.as_inline_table()
                    .is_some_and(|t| binding_is(t, chords, command))
            });
            changed |= arr.len() != before;
        } else if let Some(aot) = item.as_array_of_tables_mut() {
            let before = aot.len();
            aot.retain(|t| !binding_is(t, chords, command));
            changed |= aot.len() != before;
        }
        // Any other shape: nothing to remove, and nothing this function can
        // fix — the no-op stands (see the rustdoc).
    }
    if changed {
        write_config_file(&lock, &doc)?;
    }
    Ok(KeymapWrite { path, changed })
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

/// `[ui] panel_bar_style`: how the panel bar names its buttons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PanelBarStyle {
    /// The localized panel name with its access letter underlined. Default.
    #[default]
    Names,
    /// Only the access letter — the row's original form.
    Letters,
}

impl PanelBarStyle {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Names => "names",
            Self::Letters => "letters",
        }
    }
}

/// `[ui] date_format`: the default format of the `mtime` column.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DateFormat {
    /// The time today, day and time this year, the date before that. Default.
    #[default]
    Smart,
    /// `11h ago`.
    Relative,
    /// `2026-09-10 14:02`.
    Iso,
}

impl DateFormat {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Smart => "smart",
            Self::Relative => "relative",
            Self::Iso => "iso",
        }
    }
}

/// `[ui] splash`: what the startup screen does (spec 2026-09-15, phase 2).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SplashMode {
    /// A cover over the first frame that any key — or the first listing plus a
    /// moment — takes away. Default: it says which build is running without
    /// standing between the reader and their files.
    #[default]
    Brief,
    /// No splash at all.
    Off,
    /// A start screen that stays until a key: recent and popular directories,
    /// bookmarks and profiles, each reachable by number.
    Home,
}

impl SplashMode {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Brief => "brief",
            Self::Off => "off",
            Self::Home => "home",
        }
    }
}

/// `[ui] processes_panel`: whether the processes panel opens by itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ProcessesPanel {
    /// Opens when a task starts and closes when the last one is gone.
    /// Default: a panel that says "nothing running" is a third of the screen
    /// saying nothing.
    #[default]
    Auto,
    /// Only the command and the panel bar open or close it.
    Manual,
}

impl ProcessesPanel {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Manual => "manual",
        }
    }
}

/// `[ui] dir_indicator`: the `/` a directory row is prefixed with.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DirIndicator {
    /// The slash only when the icon column is closed. Default: an icon
    /// already says what the row is, and then the slash is noise.
    #[default]
    Auto,
    /// Always, the way it has always been painted.
    Slash,
    /// Never.
    None,
}

impl DirIndicator {
    /// The wire string this variant round-trips from/to.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Slash => "slash",
            Self::None => "none",
        }
    }
}

/// The `[ui]` keys that shape the CHROME around the listings (the key bar,
/// the panel bar's labels, the pane footer, the date format, notice expiry
/// and dialog buttons). All presentation-only, so every layer including
/// Project is honored, last-present-wins per key. One struct rather than six
/// more fields because the coverage sweeps destructure `CommonConfig` field
/// by field and clippy caps the merge helpers' argument lists.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct UiChrome {
    /// `[ui] key_bar` (None = pinned).
    pub key_bar: Option<bool>,
    /// `[ui] panel_bar_style` (None = names), validated.
    pub panel_bar_style: Option<PanelBarStyle>,
    /// `[ui] pane_footer` (None = shown).
    pub pane_footer: Option<bool>,
    /// `[ui] date_format` (None = smart), validated.
    pub date_format: Option<DateFormat>,
    /// `[ui] notice_seconds` (None = 8; 0 = until the next key), at most 600.
    pub notice_seconds: Option<u32>,
    /// `[ui] dialog_buttons` (None = buttons).
    pub dialog_buttons: Option<bool>,
    /// `[ui] history_size` (None = 30), within `5..=64`.
    pub history_size: Option<u32>,
    /// `[ui] splash` (None = brief), validated.
    pub splash: Option<SplashMode>,
    /// `[ui] splash_ms` (None = 4000): how long `brief` covers the first
    /// frame, in milliseconds, within `200..=60_000`.
    ///
    /// A cover that a reader cannot finish reading is a cover that only gets
    /// in the way, and 1200 ms — what this used to be, fixed — was not enough
    /// to take in the build and the core it talks to. Whoever wants it gone
    /// has `off`; whoever wants it to stay has `home`.
    pub splash_ms: Option<u32>,
    /// `[ui] processes_panel` (None = auto), validated.
    pub processes_panel: Option<ProcessesPanel>,
    /// `[ui] dir_indicator` (None = auto), validated.
    pub dir_indicator: Option<DirIndicator>,
}

impl UiChrome {
    /// The lowest `history_size`: below it `nav.back` stops being a trail.
    pub const MIN_HISTORY_SIZE: u32 = 5;
    /// The highest `history_size`: what the saved session keeps per panel
    /// (`norte_frontend::session::HISTORY_CAP`, pinned equal by a test there).
    pub const MAX_HISTORY_SIZE: u32 = 64;
    /// What `history_size` means when absent.
    pub const DEFAULT_HISTORY_SIZE: u32 = 30;

    /// Effective `history_size` (absent = 30).
    #[must_use]
    pub fn history_size(self) -> usize {
        self.history_size.unwrap_or(Self::DEFAULT_HISTORY_SIZE) as usize
    }

    /// The shortest `splash_ms`: below it the cover is a flash, not a screen.
    pub const MIN_SPLASH_MS: u32 = 200;
    /// The longest `splash_ms`: a minute of cover is `home` with extra steps,
    /// and `home` is the mode that stays until a key.
    pub const MAX_SPLASH_MS: u32 = 60_000;
    /// What `splash_ms` means when absent.
    ///
    /// It is also what `norte_frontend::splash::BRIEF_MS` reports, derived
    /// from here rather than written twice: two numbers that must agree, with
    /// nothing forcing them to, is how they stop agreeing.
    pub const DEFAULT_SPLASH_MS: u32 = 4_000;

    /// Effective `splash` (absent = brief).
    #[must_use]
    pub fn splash(self) -> SplashMode {
        self.splash.unwrap_or_default()
    }

    /// Effective `splash_ms` (absent = 4000).
    #[must_use]
    pub fn splash_ms(self) -> u32 {
        self.splash_ms.unwrap_or(Self::DEFAULT_SPLASH_MS)
    }

    /// Effective `processes_panel` (absent = auto).
    #[must_use]
    pub fn processes_panel(self) -> ProcessesPanel {
        self.processes_panel.unwrap_or_default()
    }

    /// Effective `dir_indicator` (absent = auto).
    #[must_use]
    pub fn dir_indicator(self) -> DirIndicator {
        self.dir_indicator.unwrap_or_default()
    }

    /// The upper bound of `notice_seconds`: ten minutes is already "never
    /// goes away on its own" in practice, and a larger number is a typo.
    pub const MAX_NOTICE_SECONDS: u32 = 600;
    /// What `notice_seconds` means when absent.
    pub const DEFAULT_NOTICE_SECONDS: u32 = 8;

    /// Effective `key_bar` (absent = pinned).
    #[must_use]
    pub fn key_bar(self) -> bool {
        self.key_bar.unwrap_or(true)
    }
    /// Effective `panel_bar_style` (absent = names).
    #[must_use]
    pub fn panel_bar_style(self) -> PanelBarStyle {
        self.panel_bar_style.unwrap_or_default()
    }
    /// Effective `pane_footer` (absent = shown).
    #[must_use]
    pub fn pane_footer(self) -> bool {
        self.pane_footer.unwrap_or(true)
    }
    /// Effective `date_format` (absent = smart).
    #[must_use]
    pub fn date_format(self) -> DateFormat {
        self.date_format.unwrap_or_default()
    }
    /// Effective `notice_seconds` (absent = 8).
    #[must_use]
    pub fn notice_seconds(self) -> u32 {
        self.notice_seconds.unwrap_or(Self::DEFAULT_NOTICE_SECONDS)
    }
    /// Effective `dialog_buttons` (absent = buttons).
    #[must_use]
    pub fn dialog_buttons(self) -> bool {
        self.dialog_buttons.unwrap_or(true)
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
    /// Provider selected for embeddings (`index.embed` /
    /// `index.search_semantic`). `None` = no embeddings.
    pub embed_provider: Option<String>,
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
    /// Extensión del nombre (#138).
    Extension,
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
    /// `[ui] theme_light` (last-wins; None = no light variant): the theme
    /// the window paints when the desktop prefers a light scheme. The
    /// terminal ignores it: a terminal has no scheme to ask.
    pub ui_theme_light: Option<String>,
    /// `[ui] theme_dark` (last-wins; None = no dark variant); see
    /// [`Self::ui_theme_light`].
    pub ui_theme_dark: Option<String>,
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
    /// `[ui] layout` (last-wins; None = el preset `orthodox`). Nombre de un
    /// fichero en `layouts/`. Presentación-solo, como el resto de escalares
    /// `[ui]`: repartir la pantalla no lanza, escribe ni redirige nada.
    pub ui_layout: Option<String>,
    /// `[ui] mouse` (last-wins; None = captured). Honored from ALL layers
    /// including Project — presentation-only, same class as the other
    /// `[ui]` scalars above: capturing (or not capturing) the pointer
    /// cannot launch, write, or redirect anything.
    pub ui_mouse: Option<bool>,
    /// `[ui] alt_menu` (last-wins; None = off). Presentación-solo, todas las
    /// capas: pedirle al terminal un protocolo de teclado no lanza, escribe
    /// ni redirige nada.
    pub ui_alt_menu: Option<bool>,
    /// `[ui] menu_bar` (last-wins; None = FIJADA). Presentación-solo, todas
    /// las capas: una barra de menú no lanza, escribe ni redirige nada.
    ///
    /// Encendida por defecto porque el menú era la única puerta a varios
    /// comandos y no había nada en pantalla diciendo que existía: quien no se
    /// sabe `Alt+M` no puede encontrar lo que no ve.
    pub ui_menu_bar: Option<bool>,
    /// `[ui] panel_bar` (last-wins; None = FIJADA). Presentación-solo, todas
    /// las capas, mismo criterio que la de menús.
    ///
    /// Encendida por defecto por lo mismo: los paneles laterales se abrían por
    /// atajo, por menú o por paleta, y los tres exigen SABER que el panel
    /// existe. Un panel aportado por un plugin, además, no lo descubría nadie.
    pub ui_panel_bar: Option<bool>,
    /// `[ui] parent_entry` (last-wins; None = ENCENDIDA). Presentación-solo,
    /// todas las capas: una fila que sube un directorio no lanza, escribe ni
    /// redirige nada.
    ///
    /// Encendida por defecto porque es lo que espera quien viene de cualquier
    /// gestor de la familia. Nunca es un OPERANDO: con el cursor encima no hay
    /// nada señalado, así que una copia o un borrado no tienen sobre qué
    /// actuar en vez de actuar sobre el directorio padre.
    pub ui_parent_entry: Option<bool>,
    /// `[ui] editor` (last-wins; None = `$VISUAL`/`$EDITOR`/fallback POSIX).
    ///
    /// Plantilla de argv con los códigos de campo de `openers.toml` (`%f` el
    /// fichero, `%d` el directorio del pane). **Jamás desde la capa de
    /// proyecto**: nombra un programa que se ejecuta, así que un repo ajeno no
    /// elige qué corre al pulsar F4 — misma regla fail-closed que `[daemon]` y
    /// que `openers.toml`.
    pub ui_editor: Option<Vec<String>>,
    /// `[ui] editor_detached` (last-wins; None = `false`): ese editor abre
    /// VENTANA propia, así que no se suspende el frontend esperándolo. Misma
    /// capa fail-closed que [`Self::ui_editor`].
    pub ui_editor_detached: Option<bool>,
    /// `[ui] diff` (last-wins; None = `diff -u`, esperando una tecla).
    ///
    /// Plantilla de argv con los códigos de campo de `openers.toml` — `%F` son
    /// LOS DOS ficheros, `%d` el directorio del pane. Misma capa fail-closed
    /// que [`Self::ui_editor`]: nombra un programa que se ejecuta.
    pub ui_diff: Option<Vec<String>>,
    /// `[ui] diff_detached` (last-wins; None = `false`): ese comparador abre
    /// VENTANA propia. Misma capa fail-closed que [`Self::ui_diff`].
    pub ui_diff_detached: Option<bool>,
    /// The `[ui]` chrome keys (key bar, panel bar style, pane footer, date
    /// format, notice expiry, dialog buttons), last-wins per key from ALL
    /// layers including Project: presentation-only, like the scalars above.
    pub ui_chrome: UiChrome,
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
    /// `[archive] rar_delegate` (roadmap ítem 11, last-wins; never from
    /// Project — naming an executable is not presentation, and honouring it
    /// from a repository's `.norte.toml` is arbitrary code execution on `cd`).
    /// `None` = probe `PATH`.
    pub archive_rar_delegate: Option<String>,
    /// `[log] dir` (last-wins; None = `<state_dir>/logs`; never from Project —
    /// fail-closed, same reasoning as `[daemon]`: choosing where a process
    /// writes is not presentation).
    pub log_dir: Option<std::path::PathBuf>,
    /// `[log] retain` (last-wins; None = the appender's default). How many
    /// rotated files survive.
    pub log_retain: Option<usize>,
    /// `[ai]` merged (never from Project).
    pub ai: AiSettings,
    /// Files that participated (watcher + diagnostics).
    pub sources: Vec<std::path::PathBuf>,
    /// Capas de PROYECTO que no cargaron, con su motivo ya dicho.
    ///
    /// Un `.norte.toml` roto en un repositorio no puede dejar sin gestor de
    /// ficheros a quien hace `cd` ahí (#260): la capa se salta y el arranque
    /// sigue con las demás, que es lo que el usuario tenía. Vacío = todo
    /// cargó. Quien pinta lo enseña; callarlo dejaría una configuración de
    /// proyecto que el lector cree activa y no lo está.
    pub project_warnings: Vec<String>,
    /// Claves que una capa de PERFIL declaró y no puede fijar (spec
    /// 2026-08-26, D2), con su motivo ya dicho.
    ///
    /// Va aparte de [`Self::project_warnings`] a propósito: son dos
    /// procedencias distintas, y un lector no puede reaccionar igual a «tu
    /// perfil pide algo que un perfil no decide» que a «este repositorio trae
    /// una config que no se aplica». Vacío = el perfil solo pedía lo suyo.
    ///
    /// Callarlas sería lo grave: un perfil se elige de una LISTA con el
    /// programa en marcha, no como se edita una capa de configuración, y un
    /// selector que concede en silencio es un escalador de permisos.
    pub profile_warnings: Vec<String>,
    /// `[profile] title` de la capa de perfil activa, para enseñar. La
    /// IDENTIDAD del perfil es su directorio, no esto.
    pub profile_title: Option<String>,
    /// `[profile.start]` ya parseado: dónde abre cada hueco cuando el perfil
    /// todavía no tiene estado guardado.
    ///
    /// Las claves son ids de hueco de la disposición DEL PERFIL; los valores,
    /// [`VPath`]s en forma de CABLE, que es lo que escribe
    /// [`crate::save_profile`]. Lo que no parsea —ni la clave como id, ni el
    /// valor como ruta— se tira y se dice en [`Self::profile_warnings`].
    ///
    /// Un [`VPath`] y no una `PathBuf`: un hueco de un perfil puede estar en
    /// sftp o dentro de un contenedor, y esto se escribió durante meses en
    /// forma de cable mientras se leía como ruta del sistema, sin que nadie lo
    /// notara porque no lo leía nadie.
    pub profile_start: std::collections::BTreeMap<u32, norte_proto::VPath>,
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
    if let Some(v) = a.embed_provider {
        ai.embed_provider = Some(v);
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

/// Merges one layer's already-parsed `[log]` section (already filtered to
/// non-Project by the caller) into the accumulators (last-present-wins per
/// field, infallible). Extracted out of [`load`] to stay under clippy's
/// line-count cap, same pattern as [`merge_daemon_layer`].
fn merge_log_layer(
    log_dir: &mut Option<PathBuf>,
    log_retain: &mut Option<usize>,
    l: crate::schema::LogSection,
) {
    if let Some(d) = l.dir {
        *log_dir = Some(d);
    }
    if let Some(r) = l.retain {
        *log_retain = Some(r);
    }
}

/// Merges one layer's already-parsed `[archive]` section (already filtered
/// to non-Project by the caller) into the accumulators (last-present-wins
/// per field, infallible — every field is a plain scalar copy). Extracted
/// out of [`load`] to stay under clippy's line-count cap, same pattern as
/// [`merge_ai_layer`].
fn merge_archive_layer(acc: &mut ArchiveAccum, a: &crate::schema::ArchiveSection) {
    if let Some(n) = a.max_entries {
        acc.max_entries = Some(n);
    }
    if let Some(b) = a.max_decompressed_bytes {
        acc.max_decompressed_bytes = Some(b);
    }
    if let Some(n) = a.max_nesting {
        acc.max_nesting = Some(n);
    }
    if let Some(d) = a.rar_delegate.as_ref() {
        acc.rar_delegate = Some(d.clone());
    }
}

/// Los acumuladores de `[archive]` mientras [`load`] recorre las capas. Van
/// juntos porque se pasan juntos: un parámetro por campo hacía crecer la
/// firma de [`merge_archive_layer`] con cada clave nueva.
#[derive(Default)]
struct ArchiveAccum {
    max_entries: Option<u64>,
    max_decompressed_bytes: Option<u64>,
    max_nesting: Option<usize>,
    rar_delegate: Option<String>,
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
#[expect(
    clippy::too_many_arguments,
    reason = "fusión campo a campo de las fuentes de la UI"
)]
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

/// Merges one layer's `[ui]` BOOLEANS (`show_hidden`, `mouse`) into the
/// accumulators (last-present-wins). No validation: any bool is valid, and
/// both are presentation-only, so every layer including Project is honored
/// (same class as `theme`/`lang`).
///
/// A function of its own for the same reason as [`merge_ui_fonts`]: [`load`]
/// is a single pass over the layers and clippy caps its length, so each new
/// key has to bring its own merge rather than another line in the loop.
#[expect(
    clippy::too_many_arguments,
    reason = "un acumulador por clave de `[ui]`: plegarlos en un struct movería el problema a `load`"
)]
fn merge_ui_flags(
    ui_show_hidden: &mut Option<bool>,
    ui_mouse: &mut Option<bool>,
    ui_alt_menu: &mut Option<bool>,
    ui_menu_bar: &mut Option<bool>,
    ui_panel_bar: &mut Option<bool>,
    ui_parent_entry: &mut Option<bool>,
    ui_layout: &mut Option<String>,
    ui: &crate::schema::UiSection,
) {
    *ui_show_hidden = ui.show_hidden.or(*ui_show_hidden);
    *ui_mouse = ui.mouse.or(*ui_mouse);
    *ui_alt_menu = ui.alt_menu.or(*ui_alt_menu);
    *ui_menu_bar = ui.menu_bar.or(*ui_menu_bar);
    *ui_panel_bar = ui.panel_bar.or(*ui_panel_bar);
    *ui_parent_entry = ui.parent_entry.or(*ui_parent_entry);
    *ui_layout = ui.layout.clone().or(ui_layout.take());
}

/// Merges one layer's `[ui]` CHROME keys into the accumulator
/// (last-present-wins per key), validating the two enums and the bound of
/// `notice_seconds` so the diagnostic can name the source file. Same
/// #73 caution as `parse_confirm_quit`: the message names the valid values,
/// never the raw one.
///
/// # Errors
/// [`ConfigError::Toml`] on an unknown `panel_bar_style`/`date_format` or a
/// `notice_seconds` above [`UiChrome::MAX_NOTICE_SECONDS`].
fn merge_ui_chrome(
    acc: &mut UiChrome,
    ui: &crate::schema::UiSection,
    norte: &Path,
) -> Result<(), ConfigError> {
    let bad = |message: &str| ConfigError::Toml {
        path: norte.to_path_buf(),
        message: message.to_owned(),
    };
    acc.key_bar = ui.key_bar.or(acc.key_bar);
    acc.pane_footer = ui.pane_footer.or(acc.pane_footer);
    acc.dialog_buttons = ui.dialog_buttons.or(acc.dialog_buttons);
    if let Some(raw) = &ui.panel_bar_style {
        acc.panel_bar_style = Some(match raw.as_str() {
            "names" => PanelBarStyle::Names,
            "letters" => PanelBarStyle::Letters,
            _ => {
                return Err(bad(
                    "[ui] panel_bar_style inválido: solo se admite «names» o «letters»",
                ));
            }
        });
    }
    if let Some(raw) = &ui.date_format {
        acc.date_format = Some(match raw.as_str() {
            "smart" => DateFormat::Smart,
            "relative" => DateFormat::Relative,
            "iso" => DateFormat::Iso,
            _ => {
                return Err(bad(
                    "[ui] date_format inválido: solo se admite «smart», «relative» o «iso»",
                ));
            }
        });
    }
    if let Some(n) = ui.notice_seconds {
        if n > UiChrome::MAX_NOTICE_SECONDS {
            return Err(bad("[ui] notice_seconds inválido: el máximo es 600"));
        }
        acc.notice_seconds = Some(n);
    }
    if let Some(n) = ui.history_size {
        if !(UiChrome::MIN_HISTORY_SIZE..=UiChrome::MAX_HISTORY_SIZE).contains(&n) {
            return Err(bad("[ui] history_size inválido: entre 5 y 64"));
        }
        acc.history_size = Some(n);
    }
    if let Some(n) = ui.splash_ms {
        if !(UiChrome::MIN_SPLASH_MS..=UiChrome::MAX_SPLASH_MS).contains(&n) {
            return Err(bad("[ui] splash_ms inválido: entre 200 y 60000"));
        }
        acc.splash_ms = Some(n);
    }
    if let Some(raw) = &ui.splash {
        acc.splash = Some(match raw.as_str() {
            "brief" => SplashMode::Brief,
            "off" => SplashMode::Off,
            "home" => SplashMode::Home,
            _ => {
                return Err(bad(
                    "[ui] splash inválido: solo se admite «brief», «off» o «home»",
                ));
            }
        });
    }
    if let Some(raw) = &ui.processes_panel {
        acc.processes_panel = Some(match raw.as_str() {
            "auto" => ProcessesPanel::Auto,
            "manual" => ProcessesPanel::Manual,
            _ => {
                return Err(bad(
                    "[ui] processes_panel inválido: solo se admite «auto» o «manual»",
                ));
            }
        });
    }
    if let Some(raw) = &ui.dir_indicator {
        acc.dir_indicator = Some(match raw.as_str() {
            "auto" => DirIndicator::Auto,
            "slash" => DirIndicator::Slash,
            "none" => DirIndicator::None,
            _ => {
                return Err(bad(
                    "[ui] dir_indicator inválido: solo se admite «auto», «slash» o «none»",
                ));
            }
        });
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
            Some(f @ ("exact" | "iec" | "si" | "relative" | "iso" | "smart" | "octal" | "rwx")) => {
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
        Some("extension") => SortColumnKey::Extension,
        Some(_) => {
            return Err(ConfigError::Toml {
                path: norte.to_path_buf(),
                message: "[ui.columns] sort.column: name | size | mtime | extension".to_owned(),
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

/// `[ui] quick_search` de esta capa, o el valor de antes si la capa es de
/// PROYECTO y el valor no vale — con su aviso.
///
/// Mismo criterio que [`parse_layer`]: un repositorio ajeno no deja sin
/// gestor de ficheros a quien hace `cd` ahí (#260).
fn merge_quick_search(
    actual: QuickSearch,
    valor: &str,
    path: &std::path::Path,
    kind: Layer,
    avisos: &mut Vec<String>,
) -> Result<QuickSearch, ConfigError> {
    match parse_quick_search(valor, path) {
        Ok(v) => Ok(v),
        Err(e) if kind == Layer::Project => {
            avisos.push(e.to_string());
            Ok(actual)
        }
        Err(e) => Err(e),
    }
}

/// El error que un `norte.toml` que no parsea produce, con su diagnóstico.
fn layer_error(raw: &str, path: &std::path::Path) -> ConfigError {
    match toml::from_str::<NorteToml>(raw) {
        Ok(_) => ConfigError::Toml {
            path: path.to_path_buf(),
            message: String::new(),
        },
        Err(e) => ConfigError::Toml {
            path: path.to_path_buf(),
            message: toml_diag(raw, &e),
        },
    }
}

/// Parsea una capa. `Ok(None)` = era de PROYECTO y no parsea, así que se
/// salta.
///
/// Cualquier clave desconocida es fatal bajo `deny_unknown_fields`, así que
/// un `.norte.toml` con una errata rompía el gestor de ficheros entero al
/// hacer `cd` a ese repositorio — y quien lo escribió puede no ser quien lo
/// sufre (#260). Las capas de usuario y de sistema siguen siendo fatales:
/// ésas SÍ son suyas, y arrancar ignorándolas en silencio sería peor que no
/// arrancar.
fn parse_layer(
    raw: &str,
    path: &std::path::Path,
    kind: Layer,
) -> Result<Option<NorteToml>, ConfigError> {
    match toml::from_str::<NorteToml>(raw) {
        Ok(p) => Ok(Some(p)),
        Err(_) if kind == Layer::Project => Ok(None),
        Err(e) => Err(ConfigError::Toml {
            path: path.to_path_buf(),
            message: toml_diag(raw, &e),
        }),
    }
}

/// Qué capas pueden fijar lo que NO es presentación.
///
/// Escrito en POSITIVO a propósito. La versión anterior era
/// `*kind != Layer::Project`, y con ella añadir una variante a [`Layer`]
/// concedía en SILENCIO el transporte, la IA, los logs y los límites
/// anti-bomba a la capa nueva. Un `match` exhaustivo obliga a decidirlo cuando
/// la variante se añade, que es cuando alguien lo está pensando.
const fn manda_fuera_de_presentacion(kind: Layer) -> bool {
    match kind {
        Layer::System | Layer::User => true,
        Layer::Profile | Layer::Project => false,
    }
}

/// Si la capa es un fichero DEL USUARIO, en el sentido que importa aquí: lo
/// escribió quien lo va a sufrir.
///
/// Sistema, usuario y perfil lo son; un `./.norte` de un repositorio ajeno no.
/// Es la línea de `keymap.preset` (#260 — elegir qué tecla borra no es
/// presentación) y la de `[[hotlist]]` (un repo ajeno no inyecta favoritos en
/// la sesión de nadie), y las dos la trazan en el mismo sitio.
const fn es_capa_del_usuario(kind: Layer) -> bool {
    match kind {
        Layer::System | Layer::User | Layer::Profile => true,
        Layer::Project => false,
    }
}

/// Los avisos de una capa de PERFIL que pidió lo que un perfil no decide (D2).
///
/// Una sección AUSENTE no avisa de nada: lo que se dice es lo que el fichero
/// declaró y no se va a aplicar.
fn profile_carve_out_warnings(parsed: &NorteToml, path: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let mut di = |seccion: &str, motivo: &str| {
        out.push(format!(
            "{}: [{seccion}] no lo decide un perfil ({motivo})",
            path.display()
        ));
    };
    // Campo a campo y no comparando contra `default()`: tres de las cuatro
    // secciones no derivan `PartialEq`, y derivarlo en tipos PÚBLICOS para una
    // comprobación interna es ampliar la API por comodidad. Un campo nuevo en
    // cualquiera de ellas tiene que aparecer aquí, y el test de las cuatro
    // secciones es lo que lo va a recordar.
    if parsed.daemon.mode.is_some() || parsed.daemon.socket.is_some() {
        di("daemon", "un perfil no redirige el transporte del core");
    }
    if parsed.ai != crate::schema::AiSection::default() {
        di(
            "ai",
            "un perfil no enciende la IA ni redirige sus proveedores",
        );
    }
    if parsed.log.dir.is_some() || parsed.log.retain.is_some() {
        di("log", "un perfil no decide dónde escribe este proceso");
    }
    if parsed.archive.max_entries.is_some()
        || parsed.archive.max_decompressed_bytes.is_some()
        || parsed.archive.max_nesting.is_some()
        || parsed.archive.rar_delegate.is_some()
    {
        di("archive", "un perfil no sube los límites anti-bomba");
    }
    out
}

/// Funde el `[profile]` de una capa de PERFIL (spec 2026-08-26, D3).
///
/// Last-wins como todo lo demás. Una clave de `start` que no parsea como id de
/// hueco —o un valor que no parsea como [`VPath`]— se TIRA con su aviso, en vez
/// de tumbar el arranque: el fichero es del usuario, pero un dedazo en un id no
/// vale una negativa a arrancar, y la capa entera se perdería por una línea.
fn merge_profile_section(
    title: &mut Option<String>,
    start: &mut std::collections::BTreeMap<u32, norte_proto::VPath>,
    section: &crate::schema::ProfileSection,
    path: &std::path::Path,
    avisos: &mut Vec<String>,
) {
    if let Some(t) = &section.title {
        *title = Some(t.clone());
    }
    for (clave, valor) in &section.start {
        let Ok(id) = clave.parse::<u32>() else {
            avisos.push(format!(
                "{}: [profile.start] «{clave}» no es un id de hueco",
                path.display()
            ));
            continue;
        };
        // Un VPath en forma de CABLE, que es lo que escribe `save_profile`. No
        // una ruta del sistema: un hueco de un perfil puede estar en sftp o
        // dentro de un contenedor, y una `PathBuf` no sabe decirlo. Además
        // quita de en medio la pregunta de contra qué se resuelve un `~` o un
        // relativo — un perfil se usa en varias máquinas y en varios días, y
        // «depende de desde dónde lo lanzaste» no es una respuesta.
        match norte_proto::VPath::parse(valor) {
            Ok(v) => {
                start.insert(id, v);
            }
            // Se dice QUÉ hueco se queda sin sembrar, que es lo accionable, y
            // no el valor: repetirlo no ayuda a arreglarlo —quien lo escribió
            // lo tiene delante— y estas cadenas acaban en el panel de registro,
            // donde una ruta de más es una ruta de más. A la barra de mensajes
            // solo llega el CONTEO, que es lo que #73 acota.
            Err(_) => avisos.push(format!(
                "{}: [profile.start] el hueco {id} no trae una ruta válida",
                path.display()
            )),
        }
    }
}

/// Loads and merges every layer (ADR 0007/0035).
///
/// # Errors
/// [`ConfigError`] naming the offending file; an ABSENT layer is not an
/// error.
// `too_many_lines`: es un merge por CAPAS, y el orden de las asignaciones ES
// la semántica (última capa gana, salvo lo que el carve-out de proyecto
// excluye). Partirlo en ayudantes que se pasaran quince parámetros de salida
// escondería justo eso, y cambiaría un lint por otro
// (`too_many_arguments`). Lo que sí se ha sacado son las decisiones con
// nombre propio: `parse_layer`, `merge_quick_search` y los `merge_*_layer`.
#[expect(
    clippy::too_many_lines,
    reason = "una pasada por capa; lo que tiene nombre propio ya está fuera"
)]
pub fn load(layers: &Layers) -> Result<CommonConfig, ConfigError> {
    let mut preset: Option<String> = None;
    let mut ui_lang: Option<String> = None;
    let mut ui_theme: Option<String> = None;
    let mut ui_theme_light: Option<String> = None;
    let mut ui_theme_dark: Option<String> = None;
    let mut quick_search = QuickSearch::default();
    let mut ui_font: Option<String> = None;
    let mut ui_mono_font: Option<String> = None;
    let mut ui_font_size: Option<f32> = None;
    let mut ui_reduce_motion: Option<bool> = None;
    let mut ui_confirm_quit = ConfirmQuit::default();
    let (mut ui_show_hidden, mut ui_mouse, mut ui_menu_bar, mut ui_panel_bar) =
        (None, None, None, None);
    let mut ui_parent_entry = None;
    let mut ui_alt_menu = None;
    let mut ui_editor: Option<Vec<String>> = None;
    let mut ui_editor_detached: Option<bool> = None;
    let mut ui_diff: Option<Vec<String>> = None;
    let mut ui_diff_detached: Option<bool> = None;
    let mut ui_layout: Option<String> = None;
    let mut ui_columns = ColumnsConfig::default();
    let mut ui_chrome = UiChrome::default();
    let mut daemon_mode: Option<DaemonMode> = None;
    let mut daemon_socket: Option<PathBuf> = None;
    let mut log_dir: Option<PathBuf> = None;
    let mut log_retain: Option<usize> = None;
    let mut hotlist: Vec<HotlistItem> = Vec::new();
    let mut archive = ArchiveAccum::default();
    let mut ai = AiSettings::default();
    let mut sources = Vec::new();
    let mut project_warnings: Vec<String> = Vec::new();
    let mut profile_warnings: Vec<String> = Vec::new();
    let mut profile_title: Option<String> = None;
    let mut profile_start: std::collections::BTreeMap<u32, norte_proto::VPath> =
        std::collections::BTreeMap::new();
    for (dir, kind) in &layers.dirs {
        let norte = dir.join("norte.toml");
        if let Some(raw) = schema::read_optional(&norte)? {
            let parsed: NorteToml = match parse_layer(&raw, &norte, *kind) {
                Ok(Some(p)) => p,
                // Una capa de PROYECTO que no parsea se SALTA, con su motivo.
                Ok(None) => {
                    project_warnings.push(layer_error(&raw, &norte).to_string());
                    continue;
                }
                Err(e) => return Err(e),
            };
            if *kind == Layer::Profile {
                profile_warnings.extend(profile_carve_out_warnings(&parsed, &norte));
                merge_profile_section(
                    &mut profile_title,
                    &mut profile_start,
                    &parsed.profile,
                    &norte,
                    &mut profile_warnings,
                );
            } else if parsed.profile.title.is_some() || !parsed.profile.start.is_empty() {
                profile_warnings.push(format!(
                    "{}: [profile] solo significa algo dentro de profiles/<nombre>/",
                    norte.display()
                ));
            }
            merge_ui_flags(
                &mut ui_show_hidden,
                &mut ui_mouse,
                &mut ui_alt_menu,
                &mut ui_menu_bar,
                &mut ui_panel_bar,
                &mut ui_parent_entry,
                &mut ui_layout,
                &parsed.ui,
            );
            // `keymap.preset` NO se honra desde proyecto (#260). Está
            // acotado a los siete presets de fábrica, así que no es
            // ejecución de código — pero los presets DISCREPAN sobre qué
            // hace cada tecla: `far` ata `shift+delete` a `pane.delete` y
            // `orthodox` ata `shift+f8` a `pane.delete-permanent`. Un
            // repositorio hostil elegiría en silencio qué tecla borra, y
            // «elegir la disposición del teclado» no es presentación: es
            // decidir qué pasa cuando el lector pulsa algo. Un PERFIL sí lo
            // elige: es un fichero del usuario, no de un repositorio ajeno.
            if es_capa_del_usuario(*kind)
                && let Some(p) = parsed.keymap.preset
            {
                preset = Some(p);
            }
            // Antes de todo lo que MUEVE campos fuera de `parsed.ui`.
            merge_ui_chrome(&mut ui_chrome, &parsed.ui, &norte)?;
            if let Some(l) = parsed.ui.lang {
                ui_lang = Some(l);
            }
            if let Some(th) = parsed.ui.theme {
                ui_theme = Some(th);
            }
            if let Some(th) = parsed.ui.theme_light {
                ui_theme_light = Some(th);
            }
            if let Some(th) = parsed.ui.theme_dark {
                ui_theme_dark = Some(th);
            }
            if let Some(qs) = &parsed.ui.quick_search {
                quick_search =
                    merge_quick_search(quick_search, qs, &norte, *kind, &mut project_warnings)?;
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
            if let Some(cols) = &parsed.ui.columns {
                merge_ui_columns(&mut ui_columns, cols, &norte)?;
            }
            // TODO lo que sigue queda FUERA del alcance de la capa de
            // proyecto, y era el mismo `if` escrito cinco veces con su motivo
            // repetido; una sola vez, con los cinco motivos juntos:
            //
            // - **hotlist** (spec 2026-07-18, decisión 3): un repo ajeno no
            //   inyecta favoritos en la sesión del usuario. Un PERFIL sí los
            //   trae (spec 2026-08-26, D2), así que va por
            //   `es_capa_del_usuario` y no por el `if` de abajo.
            // - **`[archive]`** (#95.2): son los límites anti-bomba, y SUBIRLOS
            //   desarma la protección justo donde viven los contenedores
            //   hostiles.
            // - **`[daemon]`** (review MAJOR-1): no redirige el transporte del
            //   core a un socket ajeno.
            // - **`[ai]`** (ADR 0035 decisión 3): no habilita la IA ni
            //   redirige sus proveedores.
            // - **`[log]`** (roadmap ítem 9): no decide dónde escribe este
            //   proceso.
            //
            // Los ESCALARES de UI (quick_search, theme, lang) SÍ se honran
            // desde proyecto: son presentación, y ninguno de ellos lanza,
            // escribe ni redirige nada. Ésa es la línea, y `keymap.preset`
            // cae del otro lado (#260): elegir qué tecla borra no es
            // presentación. Se filtra donde se lee, más arriba.
            //
            // Y las cuatro secciones de abajo van por
            // `manda_fuera_de_presentacion`, que está escrito en POSITIVO: el
            // `!= Layer::Project` que había aquí concedía todo esto a
            // cualquier variante nueva de `Layer` sin que nadie lo decidiera.
            // La **hotlist** se separa del resto en la 0079: un perfil SÍ trae
            // sus favoritos —es un fichero del usuario y llevarlos es media
            // razón de que exista un espacio de trabajo— mientras que las
            // cuatro secciones de abajo siguen siendo suyas de nadie más que
            // sistema y usuario.
            if es_capa_del_usuario(*kind) {
                for entry in parsed.hotlist {
                    merge_hotlist_entry(&mut hotlist, entry);
                }
            }
            if manda_fuera_de_presentacion(*kind) {
                // El EDITOR también, y por el mismo motivo que `[daemon]`:
                // nombra un programa que se ejecuta, así que un repo ajeno no
                // elige qué corre al pulsar F4. Es la misma línea que deja
                // fuera a `keymap.preset` — elegir qué tecla borra no es
                // presentación, y elegir qué binario se lanza, menos.
                ui_editor = parsed.ui.editor.clone().or(ui_editor);
                ui_editor_detached = parsed.ui.editor_detached.or(ui_editor_detached);
                // El COMPARADOR (#312) entra por la misma puerta que el
                // editor: es otro programa que se ejecuta.
                ui_diff = parsed.ui.diff.clone().or(ui_diff);
                ui_diff_detached = parsed.ui.diff_detached.or(ui_diff_detached);
                merge_archive_layer(&mut archive, &parsed.archive);
                merge_daemon_layer(&mut daemon_mode, &mut daemon_socket, parsed.daemon);
                merge_log_layer(&mut log_dir, &mut log_retain, parsed.log);
                merge_ai_layer(&mut ai, parsed.ai, &norte)?;
            }
            sources.push(norte);
        }
    }
    Ok(CommonConfig {
        preset: preset.unwrap_or_else(|| schema::DEFAULT_PRESET.to_owned()),
        ui_lang,
        ui_theme,
        ui_theme_light,
        ui_theme_dark,
        quick_search,
        ui_font,
        ui_mono_font,
        ui_font_size,
        ui_reduce_motion,
        ui_confirm_quit,
        ui_show_hidden,
        ui_layout,
        ui_mouse,
        ui_alt_menu,
        ui_menu_bar,
        ui_panel_bar,
        ui_parent_entry,
        ui_editor,
        ui_editor_detached,
        ui_diff,
        ui_diff_detached,
        ui_columns,
        ui_chrome,
        daemon_mode,
        daemon_socket,
        log_dir,
        log_retain,
        hotlist,
        archive_max_entries: archive.max_entries,
        archive_max_decompressed_bytes: archive.max_decompressed_bytes,
        archive_max_nesting: archive.max_nesting,
        archive_rar_delegate: archive.rar_delegate,
        ai,
        sources,
        project_warnings,
        profile_warnings,
        profile_title,
        profile_start,
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

    /// El layer Project JAMÁS elige qué ejecutable se lanza: un repositorio
    /// que trae su propio `.norte.toml` con `[archive] rar_delegate` sería
    /// ejecución de código arbitrario con solo entrar en el directorio. Misma
    /// regla fail-closed que el resto de `[archive]`, y aquí más afilada.
    #[test]
    fn rar_delegate_del_layer_project_se_ignora() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[archive]\nrar_delegate = \"/usr/bin/7z\"\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[archive]\nrar_delegate = \"/tmp/evil\"\n",
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
            cfg.archive_rar_delegate.as_deref(),
            Some("/usr/bin/7z"),
            "el layer Project jamás elige el ejecutable"
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

    /// `[ui] mouse`: last-wins, todas las capas — misma clase
    /// presentación-solo que `show_hidden`. Ausente = None, que el frontend
    /// lee como CAPTURAR (el default va en el frontend, no aquí: la config
    /// distingue «no lo dijo» de «dijo true», y solo así un `mouse = true`
    /// explícito puede ganarle a un `false` de una capa anterior).
    #[test]
    fn ui_mouse_carga_last_wins_y_ausente_es_none() {
        let system = tempfile::tempdir().unwrap();
        std::fs::write(system.path().join("norte.toml"), "[ui]\nmouse = false\n").unwrap();
        let user = tempfile::tempdir().unwrap();
        std::fs::write(user.path().join("norte.toml"), "[ui]\nmouse = true\n").unwrap();
        let layers = Layers {
            dirs: vec![
                (system.path().to_path_buf(), Layer::System),
                (user.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_mouse, Some(true), "last-wins");

        let empty = tempfile::tempdir().unwrap();
        std::fs::write(empty.path().join("norte.toml"), "").unwrap();
        let layers = Layers {
            dirs: vec![(empty.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_mouse, None, "ausente = None (el frontend captura)");
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

    /// `[ui]` chrome: the six keys land, the two enums validate, the project
    /// layer is honored (presentation-only), and a bad value names the file.
    #[test]
    fn ui_chrome_carga_valida_y_honra_proyecto() {
        let user = tempfile::tempdir().unwrap();
        std::fs::write(
            user.path().join("norte.toml"),
            "[ui]\nkey_bar = false\npanel_bar_style = \"letters\"\ndate_format = \"iso\"\n\
             notice_seconds = 30\nhistory_size = 12\nsplash = \"home\"\n\
             processes_panel = \"manual\"\ndir_indicator = \"slash\"\n",
        )
        .unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::write(
            project.path().join("norte.toml"),
            "[ui]\npane_footer = false\ndialog_buttons = false\ndate_format = \"relative\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (user.path().to_path_buf(), Layer::User),
                (project.path().to_path_buf(), Layer::Project),
            ],
        };
        let c = load(&layers).expect("carga").ui_chrome;
        assert_eq!(c.key_bar, Some(false));
        assert_eq!(c.panel_bar_style(), PanelBarStyle::Letters);
        assert_eq!(c.date_format(), DateFormat::Relative, "la última capa gana");
        assert_eq!(c.notice_seconds(), 30);
        assert_eq!(c.history_size(), 12);
        assert_eq!(c.splash(), SplashMode::Home);
        assert_eq!(c.processes_panel(), ProcessesPanel::Manual);
        assert_eq!(c.dir_indicator(), DirIndicator::Slash);
        assert!(!c.pane_footer());
        assert!(!c.dialog_buttons());

        let empty = load(&Layers { dirs: vec![] }).expect("carga").ui_chrome;
        assert_eq!(empty, UiChrome::default());
        assert!(empty.key_bar() && empty.pane_footer() && empty.dialog_buttons());
        assert_eq!(empty.panel_bar_style(), PanelBarStyle::Names);
        assert_eq!(empty.date_format(), DateFormat::Smart);
        assert_eq!(empty.notice_seconds(), 8);
        assert_eq!(empty.history_size(), 30);
        assert_eq!(empty.splash(), SplashMode::Brief);
        assert_eq!(empty.processes_panel(), ProcessesPanel::Auto);
        assert_eq!(empty.dir_indicator(), DirIndicator::Auto);

        for bad in [
            "panel_bar_style = \"icons\"",
            "date_format = \"unix\"",
            "notice_seconds = 601",
            "history_size = 4",
            "history_size = 65",
            "splash = \"always\"",
            "processes_panel = \"si\"",
            "dir_indicator = \"arrow\"",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("norte.toml"), format!("[ui]\n{bad}\n")).unwrap();
            let layers = Layers {
                dirs: vec![(dir.path().to_path_buf(), Layer::User)],
            };
            let err = load(&layers).expect_err(bad);
            assert!(matches!(err, ConfigError::Toml { .. }), "{bad}");
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

    /// `embed_provider` (M4-IA-2) is a plain `Option` scalar like
    /// `rename_provider`: last-present-wins across layers, and absent in
    /// every layer means `None` (no embeddings).
    #[test]
    fn ai_embed_provider_last_layer_wins() {
        let sistema = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[ai]\nembed_provider = \"ollama-local\"\n",
        )
        .unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ai]\nembed_provider = \"otro\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(
            cfg.ai.embed_provider,
            Some("otro".to_owned()),
            "scalar last-present-wins"
        );

        // Absent in every layer: stays `None`.
        let vacio = tempfile::tempdir().unwrap();
        std::fs::write(vacio.path().join("norte.toml"), "[ai]\nenabled = true\n").unwrap();
        let layers = Layers {
            dirs: vec![(vacio.path().to_path_buf(), Layer::System)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ai.embed_provider, None, "absent in all layers => None");
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

    /// `[log]` se lee de las capas de máquina y de usuario, JAMÁS de la de
    /// proyecto: un `norte.toml` que llega con un repositorio ajeno no puede
    /// decidir dónde escribe sus logs este proceso. Misma regla fail-closed que
    /// `[daemon]` (review MAJOR-1) y por el mismo motivo — redirigir una
    /// escritura no es presentación.
    #[test]
    fn log_de_proyecto_se_ignora() {
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[log]\ndir = \"/de-usuario\"\nretain = 3\n",
        )
        .unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[log]\ndir = \"/del-repo\"\nretain = 99\n",
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
            cfg.log_dir.as_deref(),
            Some(std::path::Path::new("/de-usuario")),
            "gana la capa de usuario; la de proyecto ni se mira"
        );
        assert_eq!(cfg.log_retain, Some(3));
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

    /// `[ui] theme_light` / `theme_dark` (spec 2026-09-11, V6): cargan como
    /// `theme` —cadenas sin validar aquí, el frontend las resuelve— y una
    /// capa superior gana por clave, sin arrastrar la otra.
    #[test]
    fn theme_light_y_theme_dark_cargan_y_la_capa_superior_gana_por_clave() {
        let sistema = tempfile::tempdir().unwrap();
        let usuario = tempfile::tempdir().unwrap();
        std::fs::write(
            sistema.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\ntheme_light = \"gruvbox-light\"\ntheme_dark = \"gruvbox-dark\"\n",
        )
        .unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ui]\ntheme_dark = \"catppuccin-mocha\"\n",
        )
        .unwrap();
        let layers = Layers {
            dirs: vec![
                (sistema.path().to_path_buf(), Layer::System),
                (usuario.path().to_path_buf(), Layer::User),
            ],
        };
        let cfg = load(&layers).expect("load");
        assert_eq!(cfg.ui_theme.as_deref(), Some("nord"));
        assert_eq!(cfg.ui_theme_light.as_deref(), Some("gruvbox-light"));
        assert_eq!(
            cfg.ui_theme_dark.as_deref(),
            Some("catppuccin-mocha"),
            "la capa del usuario pisa solo la clave que escribe"
        );
    }

    /// El ancho comparte escritor con el formato: entra en la MISMA entrada
    /// del id (no nace una segunda), conserva el formato que había, y el
    /// `load` real lo devuelve como `WidthChoice::Fixed`.
    #[test]
    fn persist_column_width_round_tripea_por_load_y_conserva_el_formato() {
        let dir = tempfile::tempdir().unwrap();
        persist_column_format(dir.path(), "size", "si").expect("format");
        persist_column_width(dir.path(), "size", 12).expect("width");
        persist_column_width(dir.path(), "size", 14).expect("width otra vez");
        let s = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        assert_eq!(s.matches(r#"id = "size""#).count(), 1, "UNA entrada: {s}");
        assert!(s.contains("fixed = 14"), "{s}");
        assert!(!s.contains("fixed = 12"), "sin valor viejo: {s}");
        let layers = Layers {
            dirs: vec![(dir.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("load");
        let spec = cfg.ui_columns.specs.get("size").expect("spec");
        assert_eq!(spec.width, Some(WidthChoice::Fixed(14)));
        assert_eq!(spec.format.as_deref(), Some("si"));
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
        // Mismo open SIN truncar que `lock_config_file` (review MAJOR-1).
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

/// Tests for the `keymap.toml` writer (K3c c1) — the SECOND writable config
/// file, and the first value shape that is an array of inline TABLES. The
/// load-bearing ones are the lock (a keymap write must never take
/// `norte.toml`'s), idempotence (a double confirm cannot double-write),
/// replace-in-place (a SECOND rebind of the same key must take effect) and
/// the refusal to extend a file that would not load.
#[cfg(test)]
mod persist_keymap_tests {
    use super::*;

    /// `&["g", "g"]` as the writer wants it.
    fn seq(cs: &[&str]) -> Vec<String> {
        cs.iter().map(|c| (*c).to_owned()).collect()
    }

    /// Reads back `dir/keymap.toml`.
    fn read(dir: &std::path::Path) -> String {
        std::fs::read_to_string(dir.join("keymap.toml")).expect("keymap.toml")
    }

    /// Neither the file nor the dir exist: both are created, and the binding
    /// lands under an EXPLICIT `[pane]` with the presets' one-per-line shape.
    #[test]
    fn bind_creates_the_file_the_dir_and_the_section() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("subdir/not-yet");
        let w = persist_keymap_bind(
            &dir,
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        assert!(w.changed, "the first bind writes");
        assert_eq!(w.path, dir.join("keymap.toml"));
        let s = std::fs::read_to_string(&w.path).unwrap();
        assert!(s.contains("[pane]"), "explicit section: {s}");
        assert!(
            s.contains(r#"{ on = ["g", "g"], run = "cursor.top" }"#),
            "{s}"
        );
        let doc: toml_edit::DocumentMut = s.parse().expect("the written file parses");
        assert_eq!(doc["pane"]["prepend_keymap"].as_array().unwrap().len(), 1);
    }

    /// The list is the caller's choice and it is not cosmetic: a rebind needs
    /// `Prepend` (it wins over the preset), `Append` loses to it. Each writes
    /// into ITS key and neither touches the other.
    #[test]
    fn each_list_is_written_under_its_own_key() {
        let dir = tempfile::tempdir().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["f5"]),
            "pane.copy",
        )
        .unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Append,
            &seq(&["f6"]),
            "pane.move",
        )
        .unwrap();
        let doc: toml_edit::DocumentMut = read(dir.path()).parse().unwrap();
        let prepend = doc["pane"]["prepend_keymap"].as_array().unwrap();
        let append = doc["pane"]["append_keymap"].as_array().unwrap();
        assert_eq!(prepend.len(), 1, "{prepend}");
        assert_eq!(append.len(), 1, "{append}");
        assert!(prepend.to_string().contains("pane.copy"));
        assert!(append.to_string().contains("pane.move"));
    }

    /// A repeated bind (the double confirm) writes NOTHING: same bytes, and
    /// `changed == false` says so instead of the caller having to diff.
    #[test]
    fn bind_is_idempotent_and_rewrites_nothing() {
        let dir = tempfile::tempdir().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        let before = read(dir.path());
        let w = persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        assert!(!w.changed, "the binding was already there");
        assert_eq!(read(dir.path()), before, "not one byte moved");
        assert_eq!(before.matches("cursor.top").count(), 1, "{before}");
    }

    /// A SECOND rebind of the same chord REPLACES the first in place. Appending
    /// instead would leave two entries for one chord with the older one
    /// winning (first wins, in file order): the user's second choice would do
    /// nothing, with the editor reporting success.
    #[test]
    fn rebinding_a_chord_replaces_it_in_place() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nprepend_keymap = [\n    { on = [\"f9\"], run = \"pane.mkdir\" },\n    { on = [\"g\"], run = \"cursor.top\" },\n]\n",
        )
        .unwrap();
        let w = persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.bottom",
        )
        .unwrap();
        assert!(w.changed);
        let s = read(dir.path());
        assert!(!s.contains("cursor.top"), "the old command is gone: {s}");
        assert_eq!(s.matches("[\"g\"]").count(), 1, "ONE entry for `g`: {s}");
        let doc: toml_edit::DocumentMut = s.parse().unwrap();
        let arr = doc["pane"]["prepend_keymap"].as_array().unwrap();
        assert_eq!(arr.len(), 2, "the sibling stays");
        // Position preserved: `g` is still the second row, not moved to the end.
        assert_eq!(
            arr.get(1).unwrap().as_inline_table().unwrap()["run"].as_str(),
            Some("cursor.bottom")
        );
    }

    /// Same chord, other section or other list: different binding, no replace.
    #[test]
    fn a_chord_bound_elsewhere_is_not_replaced() {
        let dir = tempfile::tempdir().unwrap();
        for (section, list) in [
            ("pane", KeymapList::Prepend),
            ("pane", KeymapList::Append),
            ("viewer", KeymapList::Prepend),
            ("global", KeymapList::Prepend),
            ("dialog", KeymapList::Prepend),
        ] {
            assert!(
                persist_keymap_bind(dir.path(), section, list, &seq(&["g"]), "cursor.top")
                    .unwrap()
                    .changed,
                "{section}/{}",
                list.key()
            );
        }
        let s = read(dir.path());
        assert_eq!(s.matches("cursor.top").count(), 5, "{s}");
    }

    /// Binding into an existing layer keeps its comments, its formatting and
    /// every binding that was already there — and the comment that annotated
    /// the LAST binding stays on that binding's line instead of being handed
    /// to the new one.
    #[test]
    fn bind_keeps_comments_on_the_binding_they_annotate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "# my keymap\n[pane]\nprepend_keymap = [\n    { on = [\"f9\"], run = \"pane.mkdir\" }, # mine\n]\n",
        )
        .unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        let s = read(dir.path());
        assert!(s.contains("# my keymap"), "file comment: {s}");
        assert!(s.contains("pane.mkdir"), "previous binding intact: {s}");
        assert!(s.contains("cursor.top"), "{s}");
        let annotated = s
            .lines()
            .find(|l| l.contains("# mine"))
            .expect("the comment survives");
        assert!(
            annotated.contains("pane.mkdir"),
            "the comment stays on the binding it annotates, it does not migrate: {s}"
        );
    }

    /// The array-of-tables shape (`[[pane.append_keymap]]`) is legal TOML and
    /// serde reads it: this writer must extend it in place, see the binding
    /// that is already there instead of writing a duplicate in the other
    /// shape, and replace in place there too.
    #[test]
    fn an_array_of_tables_layer_is_written_in_its_own_shape() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[[pane.prepend_keymap]]\non = [\"f9\"]\nrun = \"pane.mkdir\"\n",
        )
        .unwrap();
        assert!(
            !persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["f9"]),
                "pane.mkdir"
            )
            .unwrap()
            .changed,
            "already bound, in the other shape"
        );
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g", "g"]),
            "cursor.top",
        )
        .unwrap();
        let s = read(dir.path());
        assert_eq!(
            s.matches("[[pane.prepend_keymap]]").count(),
            2,
            "extended in its own shape: {s}"
        );
        // Replace in place reaches the AoT shape too.
        assert!(
            persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["f9"]),
                "pane.pack"
            )
            .unwrap()
            .changed
        );
        let s = read(dir.path());
        assert!(!s.contains("pane.mkdir"), "{s}");
        assert_eq!(s.matches("[[pane.prepend_keymap]]").count(), 2, "{s}");
        // And the unbind reaches it there.
        assert!(
            persist_keymap_unbind(dir.path(), "pane", &seq(&["f9"]), "pane.pack")
                .unwrap()
                .changed
        );
        assert!(!read(dir.path()).contains("pane.pack"));
    }

    /// A section outside the CLOSED vocabulary of keymap contexts is refused
    /// before anything is opened: `KeymapFile` is `deny_unknown_fields`, so
    /// `[panel]` would not be ignored — it would make the whole layer fail to
    /// load and cost the user their keymap.
    #[test]
    fn an_unknown_section_is_refused_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let err = persist_keymap_bind(
            dir.path(),
            "panel",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect_err("`panel` is not a keymap context");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        assert!(
            !dir.path().join("keymap.toml").exists(),
            "nothing is created on the refusal path"
        );
        assert!(
            !dir.path().join("keymap.toml.lock").exists(),
            "not even the lock: the vocabulary is checked first"
        );
    }

    /// An empty chord sequence, an empty token or an empty command are
    /// refused: none of them can make a binding under ANY chord grammar.
    #[test]
    fn an_empty_binding_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        for (chords, command) in [
            (Vec::new(), "cursor.top"),
            (seq(&["g", ""]), "cursor.top"),
            (seq(&["g"]), ""),
        ] {
            let err =
                persist_keymap_bind(dir.path(), "pane", KeymapList::Prepend, &chords, command)
                    .expect_err("empty binding");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
            let err = persist_keymap_unbind(dir.path(), "pane", &chords, command)
                .expect_err("empty binding");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        }
    }

    /// A `[pane]` that is not a table (a hand-edited file) is a CLEAN error,
    /// never the `toml_edit` index panic — same guard, and same reasoning, as
    /// `persist_set_seccion_escalar_es_err_no_panic`. The file is untouched.
    #[test]
    fn a_non_table_section_is_an_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("keymap.toml"), "pane = 3\n").unwrap();
        let err = persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect_err("[pane] scalar must be refused, not panic");
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(read(dir.path()), "pane = 3\n", "file untouched");
    }

    /// A binding list that is not a list of bindings is a clean error too —
    /// writing into it would produce a file the loader rejects.
    #[test]
    fn a_non_list_binding_list_is_an_error() {
        for src in [
            "[pane]\nprepend_keymap = 3\n",
            "[pane]\nprepend_keymap = [3]\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), src).unwrap();
            let err = persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .expect_err("not a list of bindings");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(read(dir.path()), src, "untouched");
        }
    }

    /// A file that already carries a PRESET-only key is REFUSED rather than
    /// extended: `check_layer_keys` (norte-frontend) makes `counts = true`,
    /// `dialog_from` and a non-empty `keymap` list load errors, and a layer
    /// that fails to load costs the user their whole keymap — with the editor
    /// having reported success.
    #[test]
    fn a_layer_carrying_a_preset_key_is_refused() {
        for broken in [
            "counts = true\n",
            "dialog_from = \"orthodox\"\n",
            "[pane]\nkeymap = [{ on = [\"j\"], run = \"cursor.down\" }]\n",
            "[pane]\nkeymap = 3\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), broken).unwrap();
            let err = persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .expect_err("preset key in a user layer");
            assert_eq!(err.kind(), std::io::ErrorKind::InvalidData);
            assert_eq!(read(dir.path()), broken, "file untouched: {broken}");
        }
    }

    /// The mirror of the rule above, and the reason each guard checks the
    /// VALUE the loader checks: `counts = false` and an empty `keymap = []`
    /// are legal layers that load today. A writer stricter than the loader
    /// would refuse to save into a file the app itself accepted.
    #[test]
    fn a_layer_the_loader_accepts_is_not_refused() {
        for legal in ["counts = false\n", "[pane]\nkeymap = []\n"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), legal).unwrap();
            persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .unwrap_or_else(|e| panic!("the loader accepts `{legal}`, so must the writer: {e}"));
        }
    }

    /// THE risk of reusing the `norte.toml` writers as-is: a keymap write must
    /// take `keymap.toml.lock` and nothing else. Holding `norte.toml`'s lock
    /// does not delay it, and it never creates that lock either.
    #[test]
    fn a_keymap_write_does_not_take_the_norte_toml_lock() {
        let dir = tempfile::tempdir().unwrap();
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("norte.toml.lock"))
            .unwrap();
        holder.lock().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect("norte.toml's lock must not serialise a keymap write");
        drop(holder);
        assert!(dir.path().join("keymap.toml.lock").exists(), "its own lock");
    }

    /// And the other half: a keymap write DOES wait for another writer of
    /// `keymap.toml` — the whole read-modify-write is the critical section,
    /// so two editors cannot lose each other's binding.
    #[test]
    fn a_keymap_write_waits_for_the_keymap_lock() {
        let dir = tempfile::tempdir().unwrap();
        let holder = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .open(dir.path().join("keymap.toml.lock"))
            .unwrap();
        holder.lock().unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        let d = dir.path().to_path_buf();
        let writer = std::thread::spawn(move || {
            let r =
                persist_keymap_bind(&d, "pane", KeymapList::Prepend, &seq(&["g"]), "cursor.top");
            let _ = tx.send(());
            r
        });
        assert!(
            rx.recv_timeout(std::time::Duration::from_millis(300))
                .is_err(),
            "must not complete while another writer holds the lock"
        );
        assert!(
            !dir.path().join("keymap.toml").exists(),
            "nothing written while the lock is held elsewhere"
        );
        drop(holder);
        rx.recv_timeout(std::time::Duration::from_secs(5))
            .expect("the lock released, the writer completes");
        writer.join().unwrap().expect("write");
        assert!(read(dir.path()).contains("cursor.top"));
    }

    /// Two concurrent editors do not lose each other's binding (the lock
    /// covers the read, not just the write).
    #[test]
    fn concurrent_keymap_writers_do_not_lose_bindings() {
        let dir = tempfile::tempdir().unwrap();
        let d1 = dir.path().to_path_buf();
        let d2 = dir.path().to_path_buf();
        let a = std::thread::spawn(move || {
            for i in 0..15 {
                persist_keymap_bind(
                    &d1,
                    "pane",
                    KeymapList::Prepend,
                    &seq(&[&format!("f{i}")]),
                    "cursor.top",
                )
                .expect("a");
            }
        });
        let b = std::thread::spawn(move || {
            for i in 0..15 {
                persist_keymap_bind(
                    &d2,
                    "viewer",
                    KeymapList::Append,
                    &seq(&[&format!("g{i}")]),
                    "viewer.close",
                )
                .expect("b");
            }
        });
        a.join().unwrap();
        b.join().unwrap();
        let doc: toml_edit::DocumentMut = read(dir.path()).parse().expect("parses");
        assert_eq!(doc["pane"]["prepend_keymap"].as_array().unwrap().len(), 15);
        assert_eq!(doc["viewer"]["append_keymap"].as_array().unwrap().len(), 15);
    }

    /// The tmp sibling is DERIVED from the target: a keymap write goes through
    /// `keymap.toml.tmp`. Observed deterministically — a DIRECTORY where the
    /// tmp would go makes `File::create` fail, so the write fails only if it
    /// is that name the writer picked.
    #[test]
    fn the_tmp_sibling_is_the_keymap_one() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("keymap.toml.tmp")).unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect_err("the tmp is keymap.toml.tmp");

        // ...and `norte.toml.tmp` is NOT in its way: hardcoding that name (as
        // the writer did while `norte.toml` was the only writable file) would
        // have two writers racing over one tmp.
        let other = tempfile::tempdir().unwrap();
        std::fs::create_dir(other.path().join("norte.toml.tmp")).unwrap();
        persist_keymap_bind(
            other.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .expect("norte.toml's tmp is not the keymap writer's");
    }

    /// A keymap write leaves no residual tmp, and does not touch `norte.toml`.
    #[test]
    fn a_keymap_write_leaves_no_tmp_and_does_not_touch_norte_toml() {
        let dir = tempfile::tempdir().unwrap();
        persist_set(dir.path(), "ui", "theme", toml_edit::Value::from("nord")).unwrap();
        let norte = std::fs::read_to_string(dir.path().join("norte.toml")).unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .unwrap();
        persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top").unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("norte.toml")).unwrap(),
            norte,
            "the scalar layer is untouched"
        );
        let residual: Vec<String> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains("tmp"))
            .collect();
        assert!(residual.is_empty(), "residual tmp: {residual:?}");
    }

    /// Unbind takes the binding out of BOTH user lists and leaves everything
    /// else where it was. Both, because "unbound" must be true afterwards: a
    /// copy left in the other list would keep the key firing.
    #[test]
    fn unbind_reaches_both_lists_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "# my keymap\n[pane]\nprepend_keymap = [{ on = [\"g\"], run = \"cursor.top\" }]\nappend_keymap = [\n    { on = [\"g\"], run = \"cursor.top\" },\n    { on = [\"f9\"], run = \"pane.mkdir\" },\n]\n",
        )
        .unwrap();
        let w = persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top").unwrap();
        assert!(w.changed);
        let s = read(dir.path());
        assert!(!s.contains("cursor.top"), "gone from both lists: {s}");
        assert!(s.contains("pane.mkdir"), "the sibling stays: {s}");
        assert!(s.contains("# my keymap"), "the comment stays: {s}");
    }

    /// Every duplicate goes: leaving one behind would leave the binding in
    /// force after the editor said it was unbound.
    #[test]
    fn unbind_takes_out_every_duplicate() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("keymap.toml"),
            "[pane]\nappend_keymap = [\n    { on = [\"g\"], run = \"cursor.top\" },\n    { on = [\"g\"], run = \"cursor.top\" },\n]\n",
        )
        .unwrap();
        assert!(
            persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top")
                .unwrap()
                .changed
        );
        assert!(!read(dir.path()).contains("cursor.top"));
    }

    /// Unbind matches BOTH fields: it can only ever delete the row the editor
    /// showed, never another command that happens to share the chord.
    #[test]
    fn unbind_of_something_absent_does_not_rewrite() {
        let dir = tempfile::tempdir().unwrap();
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&["g"]),
            "cursor.top",
        )
        .unwrap();
        let before = read(dir.path());
        let mtime = std::fs::metadata(dir.path().join("keymap.toml"))
            .unwrap()
            .modified()
            .unwrap();
        for (section, chords, command) in [
            ("pane", seq(&["g"]), "cursor.bottom"),
            ("pane", seq(&["h"]), "cursor.top"),
            ("viewer", seq(&["g"]), "cursor.top"),
        ] {
            let w = persist_keymap_unbind(dir.path(), section, &chords, command).unwrap();
            assert!(!w.changed, "nothing matched");
        }
        assert_eq!(read(dir.path()), before);
        assert_eq!(
            std::fs::metadata(dir.path().join("keymap.toml"))
                .unwrap()
                .modified()
                .unwrap(),
            mtime,
            "not even the mtime moved"
        );
    }

    /// No file, no dir, nothing to remove: a documented no-op that creates
    /// neither the file nor the lock's directory.
    #[test]
    fn unbind_without_a_file_is_a_no_op() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("never-existed");
        let w = persist_keymap_unbind(&dir, "pane", &seq(&["g"]), "cursor.top").unwrap();
        assert!(!w.changed);
        assert_eq!(w.path, dir.join("keymap.toml"));
        assert!(!dir.exists(), "a removal creates nothing");

        let empty = tempfile::tempdir().unwrap();
        assert!(
            !persist_keymap_unbind(empty.path(), "pane", &seq(&["g"]), "cursor.top")
                .unwrap()
                .changed
        );
        assert!(!empty.path().join("keymap.toml").exists());
    }

    /// Encoding pin (#73 discipline, rule 1): a hostile chord token — quote,
    /// newline, an embedded TOML header and a bidi override — round-trips
    /// ESCAPED and byte-identical. `toml_edit` escapes; it never injects TOML.
    /// The chord grammar lives in the frontend, so this writer's job is only
    /// that nothing the caller hands it can break the document.
    #[test]
    fn a_hostile_chord_round_trips_escaped() {
        let dir = tempfile::tempdir().unwrap();
        let hostile = "ctrl+\"x\n[[evil]]\u{202e}y";
        persist_keymap_bind(
            dir.path(),
            "pane",
            KeymapList::Prepend,
            &seq(&[hostile]),
            "cursor.top",
        )
        .unwrap();
        let doc: toml_edit::DocumentMut = read(dir.path()).parse().expect("still parses");
        assert_eq!(
            doc["pane"]["prepend_keymap"][0]["on"][0].as_str(),
            Some(hostile),
            "byte-identical after the round trip"
        );
        // And it is found again: idempotence compares the same bytes.
        assert!(
            !persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&[hostile]),
                "cursor.top"
            )
            .unwrap()
            .changed
        );
        assert!(
            persist_keymap_unbind(dir.path(), "pane", &seq(&[hostile]), "cursor.top")
                .unwrap()
                .changed
        );
    }

    /// #73, same discipline as `persist_helpers_no_citan_el_error_crudo_de_toml_edit`
    /// for the scalar layer: an unparseable `keymap.toml` names the FILE and
    /// never quotes its content — `toml_edit`'s own `Display` cites the
    /// offending line, and a hostile chord persisted earlier would ride it
    /// into whatever shows this `io::Error` (the status bar).
    #[test]
    fn an_unparseable_layer_names_the_file_not_its_content() {
        let hostile = "not toml \u{202e}[[pane.append_keymap]]\u{202c} = [unterminated\n";
        for op in ["bind", "unbind"] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), hostile).unwrap();
            let err = if op == "bind" {
                persist_keymap_bind(
                    dir.path(),
                    "pane",
                    KeymapList::Prepend,
                    &seq(&["g"]),
                    "cursor.top",
                )
                .unwrap_err()
            } else {
                persist_keymap_unbind(dir.path(), "pane", &seq(&["g"]), "cursor.top").unwrap_err()
            };
            let msg = err.to_string();
            assert!(
                !msg.contains("append_keymap") && !msg.contains('\u{202e}'),
                "{op}: the error must not echo the hostile document: {msg:?}"
            );
            assert!(msg.contains("keymap.toml"), "{op}: names the file: {msg:?}");
            assert_eq!(read(dir.path()), hostile, "{op}: file untouched");
        }
    }

    /// Two more section shapes a hand-written layer may legally use, and that
    /// serde reads: a DOTTED key (`pane.prepend_keymap = [...]`) and an INLINE
    /// table (`pane = { … }`). `persist_set` has the same positive pin
    /// (`persist_set_seccion_inline_table_no_se_rechaza`): a guard stricter
    /// than `toml_edit` would refuse a file the library indexes happily, and
    /// a write that reflowed either shape could stop it parsing — the
    /// top-ranked failure of this writer.
    #[test]
    fn dotted_and_inline_sections_are_extended_without_breaking() {
        for src in [
            "pane.prepend_keymap = [{ on = [\"f9\"], run = \"pane.mkdir\" }]\n",
            "pane = { prepend_keymap = [{ on = [\"f9\"], run = \"pane.mkdir\" }] }\n",
        ] {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("keymap.toml"), src).unwrap();
            // Seen through the shape: no duplicate written.
            assert!(
                !persist_keymap_bind(
                    dir.path(),
                    "pane",
                    KeymapList::Prepend,
                    &seq(&["f9"]),
                    "pane.mkdir"
                )
                .unwrap()
                .changed,
                "{src}"
            );
            persist_keymap_bind(
                dir.path(),
                "pane",
                KeymapList::Prepend,
                &seq(&["g"]),
                "cursor.top",
            )
            .unwrap();
            let s = read(dir.path());
            let doc: toml_edit::DocumentMut = s
                .parse()
                .unwrap_or_else(|e| panic!("the extended file must still parse ({src}): {e}\n{s}"));
            let arr = doc["pane"]["prepend_keymap"]
                .as_array()
                .unwrap_or_else(|| panic!("still a binding list ({src}): {s}"));
            assert_eq!(arr.len(), 2, "{s}");
            // And `toml` (the loader's parser, not the editor's) agrees.
            let v: toml::Value = toml::from_str(&s).unwrap_or_else(|e| panic!("{e}\n{s}"));
            assert_eq!(
                v["pane"]["prepend_keymap"].as_array().unwrap().len(),
                2,
                "{s}"
            );
        }
    }
}

/// Lo que una capa de PERFIL puede y no puede decidir (spec 2026-08-26, D2).
#[cfg(test)]
mod profile_layer_tests {
    use super::*;

    fn capas(usuario: &std::path::Path, perfil: &std::path::Path) -> Layers {
        Layers {
            dirs: vec![
                (usuario.to_path_buf(), Layer::User),
                (perfil.to_path_buf(), Layer::Profile),
            ],
        }
    }

    /// D2: el recorte de proyecto estaba escrito como `!= Layer::Project`, así
    /// que una cuarta variante heredaba EN SILENCIO todo lo del usuario. Este
    /// test es el que impide que eso vuelva: un perfil no redirige el
    /// transporte, no enciende la IA, no elige dónde se escriben los logs y no
    /// sube los límites anti-bomba.
    #[test]
    fn un_perfil_no_puede_tocar_daemon_ai_log_ni_archive() {
        let usuario = tempfile::tempdir().expect("tempdir");
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            r#"
[daemon]
socket = "/tmp/ajeno.sock"
[ai]
enabled = true
[log]
dir = "/tmp/logs-ajenos"
[archive]
max_entries = 999999999
"#,
        )
        .expect("write");

        let cfg = load(&capas(usuario.path(), perfil.path())).expect("carga");

        assert_eq!(cfg.daemon_socket, None, "el transporte no se redirige");
        assert!(!cfg.ai.enabled, "la IA no se enciende sola");
        assert_eq!(cfg.log_dir, None, "los logs no se mudan");
        assert_eq!(
            cfg.archive_max_entries, None,
            "los límites anti-bomba no suben"
        );
        assert_eq!(
            cfg.profile_warnings.len(),
            4,
            "y las cuatro se DICEN: callarlas convierte el selector en un \
             escalador de permisos"
        );
        for seccion in ["daemon", "ai", "log", "archive"] {
            assert!(
                cfg.profile_warnings.iter().any(|w| w.contains(seccion)),
                "falta el aviso de [{seccion}]: {:?}",
                cfg.profile_warnings
            );
        }
    }

    /// Y lo que SÍ puede: presentación entera, más el preset de keymap y los
    /// favoritos, que la capa de proyecto no puede y ésta sí — un perfil es del
    /// usuario, un repositorio ajeno no.
    #[test]
    fn un_perfil_pisa_presentacion_keymap_y_favoritos() {
        let usuario = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\nlayout = \"orthodox\"\n[keymap]\npreset = \"orthodox\"\n",
        )
        .expect("write");
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            r#"
[ui]
theme = "solarized"
layout = "explorer"
[keymap]
preset = "far"
[[hotlist]]
name = "src"
path = "/home/u/src"
"#,
        )
        .expect("write");

        let cfg = load(&capas(usuario.path(), perfil.path())).expect("carga");

        assert_eq!(cfg.ui_theme.as_deref(), Some("solarized"));
        assert_eq!(cfg.ui_layout.as_deref(), Some("explorer"));
        assert_eq!(cfg.preset, "far");
        assert_eq!(cfg.hotlist.len(), 1);
        assert!(cfg.profile_warnings.is_empty());
    }

    /// D3: `[profile.start]` es lo que hace útil un perfil recién creado. Las
    /// claves son ids de hueco TAL Y COMO los escribe la disposición del
    /// perfil, y los valores son [`VPath`]s en forma de cable — que es lo que
    /// escribe `save_profile`, y lo que permite que un hueco de un perfil
    /// abra en sftp o dentro de un contenedor.
    #[test]
    fn profile_start_se_lee_con_sus_ids() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[profile]\ntitle = \"Trabajo\"\n\n[profile.start]\n\
             1 = \"file:///home/u/src\"\n2 = \"sftp://maquina/srv\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_title.as_deref(), Some("Trabajo"));
        assert_eq!(
            cfg.profile_start.get(&1).map(norte_proto::VPath::to_wire),
            Some("file:///home/u/src".to_owned())
        );
        assert_eq!(
            cfg.profile_start.get(&2).map(|v| v.scheme().to_owned()),
            Some("sftp".to_owned()),
            "un hueco de un perfil no tiene por qué ser local"
        );
        assert_eq!(cfg.profile_start.len(), 2);
    }

    /// Una ruta SIN esquema se tira con su aviso, igual que una clave mala.
    ///
    /// Ese aviso llega a la pantalla (`profile_warnings`), que es lo que hace
    /// que esto sea una regla y no una trampa: quien escriba `/tmp` a mano lo
    /// ve, en vez de quedarse con un hueco que abre donde le parece.
    #[test]
    fn una_ruta_de_start_sin_esquema_se_avisa_y_se_tira() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            // Un valor con una palabra que no puede salir de ninguna otra
            // parte del aviso: el `tempdir` de este test vive DENTRO de /tmp,
            // así que buscar «/tmp» habría dado un falso positivo con la ruta
            // del propio fichero.
            "[profile.start]\n1 = \"/secreto-del-lector\"\n2 = \"file:///home/u\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_start.len(), 1, "el bueno sobrevive");
        assert!(cfg.profile_start.contains_key(&2));
        let aviso = cfg.profile_warnings.join(" ");
        assert!(
            aviso.contains("hueco 1"),
            "se dice qué hueco se quedó sin sembrar: {aviso}"
        );
        assert!(
            !aviso.contains("secreto-del-lector"),
            "y NO se cita el valor, que es una ruta y esto va a la barra: {aviso}"
        );
    }

    /// Una clave que no es un id de hueco no rompe el arranque: se tira y se
    /// dice. El fichero es del usuario, pero un dedazo en un id no vale una
    /// negativa a arrancar.
    #[test]
    fn una_clave_de_start_que_no_es_un_id_se_avisa_y_se_tira() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[profile.start]\nizquierda = \"file:///tmp\"\n1 = \"file:///home/u\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(perfil.path().to_path_buf(), Layer::Profile)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_start.len(), 1, "el bueno sobrevive");
        assert!(
            cfg.profile_warnings.iter().any(|w| w.contains("izquierda")),
            "y el malo se dice por su nombre: {:?}",
            cfg.profile_warnings
        );
    }

    /// `[profile]` en una capa que NO es de perfil no significa nada, y decirlo
    /// evita que alguien lo escriba en su norte.toml y espere que pase algo.
    #[test]
    fn profile_fuera_de_un_perfil_se_ignora_con_aviso() {
        let usuario = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[profile]\ntitle = \"no soy un perfil\"\n",
        )
        .expect("write");
        let layers = Layers {
            dirs: vec![(usuario.path().to_path_buf(), Layer::User)],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.profile_title, None);
        assert_eq!(cfg.profile_warnings.len(), 1);
    }

    /// Y el proyecto sigue mandando sobre el perfil (D1): esto es lo que hace
    /// que ADR 0026 y #260 no cambien de significado.
    #[test]
    fn proyecto_sigue_pisando_al_perfil_en_presentacion() {
        let perfil = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            perfil.path().join("norte.toml"),
            "[ui]\ntheme = \"solarized\"\n",
        )
        .expect("write");
        let proyecto = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\n",
        )
        .expect("write");

        let layers = Layers {
            dirs: vec![
                (perfil.path().to_path_buf(), Layer::Profile),
                (proyecto.path().to_path_buf(), Layer::Project),
            ],
        };
        let cfg = load(&layers).expect("carga");
        assert_eq!(cfg.ui_theme.as_deref(), Some("nord"));
    }
}

/// Lo que una capa de PROYECTO puede y no puede decidir (#260).
#[cfg(test)]
mod project_layer_tests {
    use super::*;

    fn capas(sistema: &std::path::Path, proyecto: &std::path::Path) -> Layers {
        Layers {
            dirs: vec![
                (sistema.to_path_buf(), Layer::User),
                (proyecto.to_path_buf(), Layer::Project),
            ],
        }
    }

    /// Un repositorio NO elige el preset de teclado.
    ///
    /// Está acotado a los siete de fábrica, así que no es ejecución de
    /// código — pero los presets discrepan sobre qué hace cada tecla:
    /// `far` ata `shift+delete` a `pane.delete` y `orthodox` ata `shift+f8`
    /// a `pane.delete-permanent`. Elegir cuál borra no es presentación.
    #[test]
    fn una_capa_de_proyecto_no_elige_el_preset() {
        let usuario = tempfile::tempdir().unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[keymap]\npreset = \"orthodox\"\n",
        )
        .unwrap();
        std::fs::write(
            proyecto.path().join("norte.toml"),
            "[keymap]\npreset = \"far\"\n",
        )
        .unwrap();

        let cfg = load(&capas(usuario.path(), proyecto.path())).expect("carga");
        assert_eq!(
            cfg.preset, "orthodox",
            "el preset lo elige el usuario, no el repositorio"
        );
    }

    /// Y un `.norte.toml` roto no deja a nadie sin gestor de ficheros.
    ///
    /// Cualquier clave desconocida es fatal bajo `deny_unknown_fields`, así
    /// que una errata en un repositorio ajeno rompía el arranque al hacer
    /// `cd` ahí. Ahora la capa se salta, se DICE, y lo demás sigue.
    #[test]
    fn una_capa_de_proyecto_rota_se_salta_y_se_dice() {
        let usuario = tempfile::tempdir().unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(
            usuario.path().join("norte.toml"),
            "[ui]\ntheme = \"nord\"\n",
        )
        .unwrap();
        std::fs::write(proyecto.path().join("norte.toml"), "[ui]\nno_existe = 1\n").unwrap();

        let cfg = load(&capas(usuario.path(), proyecto.path())).expect("arranca igual");
        assert_eq!(
            cfg.ui_theme.as_deref(),
            Some("nord"),
            "lo del usuario sigue"
        );
        assert_eq!(cfg.project_warnings.len(), 1, "y se dice por qué");
        assert!(
            cfg.project_warnings[0].contains("no_existe")
                || cfg.project_warnings[0].contains("norte.toml"),
            "el aviso nombra el problema: {:?}",
            cfg.project_warnings
        );
    }

    /// La capa del USUARIO sigue siendo fatal: ésa sí es suya, y arrancar
    /// ignorándola en silencio sería peor que no arrancar.
    #[test]
    fn una_capa_de_usuario_rota_sigue_siendo_fatal() {
        let usuario = tempfile::tempdir().unwrap();
        let proyecto = tempfile::tempdir().unwrap();
        std::fs::write(usuario.path().join("norte.toml"), "[ui]\nno_existe = 1\n").unwrap();

        assert!(
            load(&capas(usuario.path(), proyecto.path())).is_err(),
            "una config del usuario rota se dice a gritos"
        );
    }
}
