//! Bindings `norte.fs`/`norte.pane`/`norte.ui.message`. Paths = BYTE STRINGS
//! de Lua en entrada y salida (regla 1: cero suposición UTF-8). Relativos se
//! resuelven contra `ctx.cwd`. Toda mutación es una Task del engine (journal,
//! policy y undo). Errores del protocolo → `nil, clave` (convención Lua; la
//! clave es la ESTABLE de [`crate::app::error_key`], jamás texto localizado).
//!
//! Desviación documentada respecto a la spec (§ API v1): `norte.fs.mkdir` NO
//! se expone — ni `Backend` ni `Engine` tienen mkdir hoy (verificado
//! 2026-07-17); exponerlo aquí exigiría lógica en el frontend (regla 7) o un
//! atajo fuera del journal (regla 4). Se retira de v1 y la spec se actualiza
//! en la task 9.
//!
//! Estado compartido: `cancellers` y `messages` son `Rc<RefCell<…>>` que las
//! clausuras comparten con el driver. Los borrows son SIEMPRE puntuales
//! (push y soltar) y jamás se mantienen a través de una llamada a Lua ni de
//! un `.await` — mismo invariant que el registry de `api.rs`.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use mlua::{Lua, MultiValue, Value};
use norte_core::TransferOptions;
use norte_core::backend::{Backend, TaskCanceller};
use norte_proto::{DeleteMode, Entry, EntryKind, Error, Scheme, Segment, TaskState, VPath};

use crate::app::error_key;

/// Snapshot del estado de panes al INVOCAR el comando (congelado: la UI
/// sigue mutando mientras el script corre; determinismo > frescura).
#[derive(Debug, Clone)]
pub struct PaneCtx {
    /// Directorio del pane con foco (base de los paths relativos).
    pub cwd: VPath,
    /// Directorio del OTRO pane.
    pub other_cwd: VPath,
    /// Selección marcada en el pane con foco.
    pub selection: Vec<VPath>,
    /// Entrada bajo el cursor, si la hay.
    pub current: Option<VPath>,
}

/// Cancellers de las Tasks lanzadas por ESTE run (el driver los cancela
/// todos si el usuario aborta — regla 3).
pub type RunCancellers = Rc<RefCell<Vec<TaskCanceller>>>;

/// Tope de mensajes acumulados por run: un script en bucle no crece la cola
/// sin límite. Al alcanzarlo, los siguientes se DESCARTAN y el último
/// acumulado se sustituye por `"…"` como marca visible de desbordamiento
/// (la longitud jamás pasa del tope).
const MESSAGES_MAX: usize = 64;

/// bytes de Lua → [`VPath`].
///
/// - Con `://` = ABSOLUTO: si los bytes son UTF-8 y parsean como wire
///   ([`VPath::parse`], percent-encoding), esa es la interpretación canónica
///   (así el `path` que devuelve `list` hace round-trip). Si no (p. ej. el
///   script concatenó un `name` con bytes crudos no-UTF8), se parsea a nivel
///   de BYTES: scheme/authority en UTF-8, segmentos crudos SIN
///   percent-decodificar.
/// - Sin `://` = RELATIVO a `base`: los bytes se parten por `/` y cada
///   segmento entra CRUDO (sin percent-decoding — un nombre con `%` literal
///   no se corrompe).
///
/// Ambigüedad asumida y documentada (encoding review M4 Lua): un path
/// absoluto UTF-8 se interpreta como wire, así que un nombre que CONTENGA
/// percent-escapes válidos se decodifica — y no hace falta que sea hostil:
/// un nombre UTF-8 normal de descargas (`informe%20final.pdf`) concatenado
/// como absoluto (`cwd .. '/' .. name`) se decodificaría a
/// `informe final.pdf` y operaría sobre el fichero EQUIVOCADO. El camino
/// seguro para ABSOLUTOS es `entry.path` (la forma wire completa que
/// devuelven `list`/`stat`/`selection`: round-trip exacto); el `name`
/// crudo es para uso RELATIVO (sin `://`, donde jamás se decodifica).
///
/// NO existen las formas POSIX: ni `.`/`..` (un [`Segment`] los rechaza —
/// jamás traversal) ni `/abs` con barra inicial (sería un segmento vacío =
/// inválido). Para un absoluto, usa la forma wire completa o construye sobre
/// `norte.pane.cwd()`/`other_cwd()`.
fn to_vpath(base: &VPath, raw: &[u8]) -> Result<VPath, &'static str> {
    let invalid = || error_key(&Error::InvalidPath);
    let sep = raw.windows(3).position(|w| w == b"://");
    let Some(sep) = sep else {
        // Relativo: cada segmento crudo colgado de `base`.
        if raw.is_empty() {
            return Err(invalid());
        }
        let mut path = base.clone();
        for seg in raw.split(|&b| b == b'/') {
            path = path.join(Segment::new(seg).map_err(|_| invalid())?);
        }
        return Ok(path);
    };
    if let Ok(s) = std::str::from_utf8(raw)
        && let Ok(p) = VPath::parse(s)
    {
        return Ok(p);
    }
    parse_raw_absolute(raw, sep).ok_or_else(invalid)
}

/// Forma absoluta a nivel de BYTES: `scheme://[authority]/seg/…` con los
/// segmentos crudos (fallback de [`to_vpath`] cuando el wire no aplica).
fn parse_raw_absolute(raw: &[u8], sep: usize) -> Option<VPath> {
    let scheme = Scheme::new(std::str::from_utf8(&raw[..sep]).ok()?).ok()?;
    let rest = &raw[sep + 3..];
    let (auth_raw, path_raw) = match rest.iter().position(|&b| b == b'/') {
        Some(i) => (&rest[..i], Some(&rest[i + 1..])),
        None => (rest, None),
    };
    let authority = if auth_raw.is_empty() {
        None
    } else {
        Some(norte_proto::Authority::new(std::str::from_utf8(auth_raw).ok()?).ok()?)
    };
    let mut path = VPath::root(scheme, authority);
    if let Some(path_raw) = path_raw
        && !path_raw.is_empty()
    {
        for seg in path_raw.split(|&b| b == b'/') {
            path = path.join(Segment::new(seg).ok()?);
        }
    }
    Some(path)
}

/// Retorno Lua de éxito: un solo valor.
fn ok_mv(v: Value) -> MultiValue {
    MultiValue::from_iter([v])
}

/// Retorno Lua de error: `nil, clave` (convención estándar; la clave es
/// estable, ver [`error_key`]).
fn err_mv(lua: &Lua, key: &str) -> mlua::Result<MultiValue> {
    Ok(MultiValue::from_iter([
        Value::Nil,
        Value::String(lua.create_string(key)?),
    ]))
}

/// `kind` como string estable para Lua.
fn kind_str(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::Other => "other",
    }
}

/// Una [`Entry`] como tabla Lua: `name` = bytes CRUDOS del último segmento,
/// `path` = forma wire completa (byte string ASCII), `kind`, `size` (o nil).
fn entry_table(lua: &Lua, e: &Entry) -> mlua::Result<mlua::Table> {
    let t = lua.create_table()?;
    // La raíz de un provider no tiene último segmento: name = "" honesto.
    let name: &[u8] = e.path.file_name().map_or(b"", Segment::as_bytes);
    t.set("name", lua.create_string(name)?)?;
    t.set("path", lua.create_string(e.path.to_wire().as_bytes())?)?;
    t.set("kind", kind_str(e.kind))?;
    if let Some(size) = e.size {
        // Lua 5.4 usa enteros de 64 bits con signo; un tamaño que no cabe
        // (absurdo pero posible en un provider hostil) se omite (nil =
        // desconocido) antes que mentir con un número truncado.
        if let Ok(size) = i64::try_from(size) {
            t.set("size", size)?;
        }
    }
    Ok(t)
}

/// Desenlace terminal de una Task → retorno Lua de la mutación. Todas las
/// claves salen de [`error_key`] — cero literales duplicados del vocabulario.
fn finish(lua: &Lua, state: &TaskState) -> mlua::Result<MultiValue> {
    match state {
        TaskState::Completed => Ok(ok_mv(Value::Boolean(true))),
        TaskState::Cancelled => err_mv(lua, error_key(&Error::Cancelled)),
        TaskState::Failed { error } => err_mv(lua, error_key(error)),
        // `join` solo devuelve terminales; un estado de protocolo más nuevo
        // (`Unknown`) o uno imposible cae a err-unknown, jamás panic.
        _ => err_mv(lua, error_key(&Error::Unknown)),
    }
}

/// Instala `norte.fs`, `norte.pane` y `norte.ui.message` en la tabla `norte`
/// global YA existente (la crea `LuaHost::new`; `ui` ya existe y aquí solo
/// gana `message`). Se llama POR INVOCACIÓN (el snapshot `ctx` y los canales
/// del run cambian cada vez); reinstalar pisa las tablas anteriores.
///
/// **Consejo de API para scripts** (ambigüedad `%`, ver [`to_vpath`]): para
/// referirse a una entrada por su forma ABSOLUTA usa siempre `entry.path`
/// (wire completo, round-trip exacto); `entry.name` (bytes crudos) es para
/// construir paths RELATIVOS — concatenarlo en un absoluto decodificaría un
/// `%` literal del nombre (`informe%20final.pdf`) hacia otro fichero.
///
/// `messages` acumula los `norte.ui.message(s)` del run (bytes → String
/// lossy, tope `MESSAGES_MAX`); el CONSUMIDOR (driver, task 8) los vuelca
/// a la barra pasándolos por `detail_for_bar` (mask + tope) — aquí no se
/// sanea, se acumula.
///
/// **Ventana de Task huérfana (deuda #74, solo `Backend::Remote`):** el
/// canceller de cada mutación se registra tras volver el RPC de submit. Si
/// el driver ABANDONA el future del run (timeout duro / fin de la gracia)
/// con ese submit en vuelo, la Task nace en el daemon sin canceller
/// registrado y el abort no la cancela. NO es una fuga de gobierno: sigue
/// bajo journal/policy/undo y visible (y cancelable a mano) en el panel de
/// tasks. La reconciliación queda en la issue #74 — aquí solo se documenta.
///
/// **Hazard del stash — CERRADO por el flag `closed`:** un script puede
/// guardar `norte.fs.copy` en un global y llamarlo en un run POSTERIOR; esa
/// referencia rancia pushearía cancellers al run viejo y resolvería
/// relativos contra un `ctx` congelado obsoleto. El driver instala bindings
/// FRESCOS en cada `invoke` (esta función pisa las tablas) y, además, cada
/// binding de `norte.fs` comprueba `closed` AL ENTRAR: si el run que lo creó
/// ya terminó (el driver lo pone a `true` SIEMPRE al salir, guard RAII), el
/// binding devuelve `nil, err-unsupported` («binding de un run cerrado») —
/// mismo patrón que la bandera de `norte.command` en `api.rs`. Los bindings
/// de `norte.pane`/`norte.ui.message` no lo comprueban: son un snapshot
/// congelado / un acumulador que muere con el run — rancios pero inofensivos
/// (sin efectos sobre el FS ni sobre los cancellers).
#[allow(clippy::too_many_lines)] // wiring de bindings uno a uno, sin lógica
pub(crate) fn install_fs(
    lua: &Lua,
    backend: Backend,
    ctx: PaneCtx,
    cancellers: RunCancellers,
    messages: Rc<RefCell<Vec<String>>>,
    closed: Rc<Cell<bool>>,
) -> mlua::Result<()> {
    let norte: mlua::Table = lua.globals().get("norte")?;
    let fs = lua.create_table()?;

    // --- Lecturas -------------------------------------------------------
    {
        let backend = backend.clone();
        let cwd = ctx.cwd.clone();
        let closed = Rc::clone(&closed);
        fs.set(
            "list",
            lua.create_async_function(move |lua, path: mlua::String| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let closed = Rc::clone(&closed);
                async move {
                    // Binding de un run cerrado (stash): muerto, ver rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let dir = match to_vpath(&cwd, &path.as_bytes()) {
                        Ok(p) => p,
                        Err(key) => return err_mv(&lua, key),
                    };
                    match backend.list(&dir).await {
                        Ok(entries) => {
                            let t = lua.create_table()?;
                            for (i, e) in entries.iter().enumerate() {
                                t.set(i + 1, entry_table(&lua, e)?)?;
                            }
                            Ok(ok_mv(Value::Table(t)))
                        }
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }
    {
        let backend = backend.clone();
        let cwd = ctx.cwd.clone();
        let closed = Rc::clone(&closed);
        fs.set(
            "stat",
            lua.create_async_function(move |lua, path: mlua::String| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let closed = Rc::clone(&closed);
                async move {
                    // Binding de un run cerrado (stash): muerto, ver rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let p = match to_vpath(&cwd, &path.as_bytes()) {
                        Ok(p) => p,
                        Err(key) => return err_mv(&lua, key),
                    };
                    match backend.stat(&p).await {
                        Ok(e) => Ok(ok_mv(Value::Table(entry_table(&lua, &e)?))),
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }

    // --- Mutaciones (Tasks del engine: journal + policy + undo) ---------
    for (key, mv) in [("copy", false), ("move", true)] {
        let backend = backend.clone();
        let cwd = ctx.cwd.clone();
        let cancellers = Rc::clone(&cancellers);
        let closed = Rc::clone(&closed);
        fs.set(
            key,
            lua.create_async_function(move |lua, (src, dst): (mlua::String, mlua::String)| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let cancellers = Rc::clone(&cancellers);
                let closed = Rc::clone(&closed);
                async move {
                    // Binding de un run cerrado (stash): muerto, ver rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let (from, to) = match (
                        to_vpath(&cwd, &src.as_bytes()),
                        to_vpath(&cwd, &dst.as_bytes()),
                    ) {
                        (Ok(f), Ok(t)) => (f, t),
                        (Err(key), _) | (_, Err(key)) => return err_mv(&lua, key),
                    };
                    let submitted = if mv {
                        backend.move_(&from, &to, TransferOptions::default()).await
                    } else {
                        backend.copy(&from, &to, TransferOptions::default()).await
                    };
                    match submitted {
                        Ok(task) => {
                            // ANTES del join: si el usuario aborta el run, el
                            // driver puede cancelar esta Task en vuelo. OJO:
                            // si el driver ABANDONA el future con el submit
                            // remoto aún en vuelo, la Task nace sin canceller
                            // registrado (ventana documentada en install_fs,
                            // deuda #74).
                            cancellers.borrow_mut().push(task.canceller());
                            finish(&lua, &task.join().await)
                        }
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }
    {
        // Último uso: `backend`, `cancellers` y `closed` se MUEVEN aquí.
        let cwd = ctx.cwd.clone();
        fs.set(
            "delete",
            lua.create_async_function(move |lua, path: mlua::String| {
                let backend = backend.clone();
                let cwd = cwd.clone();
                let cancellers = Rc::clone(&cancellers);
                let closed = Rc::clone(&closed);
                async move {
                    // Binding de un run cerrado (stash): muerto, ver rustdoc.
                    if closed.get() {
                        return err_mv(&lua, error_key(&Error::Unsupported));
                    }
                    let p = match to_vpath(&cwd, &path.as_bytes()) {
                        Ok(p) => p,
                        Err(key) => return err_mv(&lua, key),
                    };
                    // SIEMPRE papelera: el permanente NO se expone en v1
                    // (spec M4 Lua) — un script no borra irreversible.
                    match backend.delete(&p, DeleteMode::Trash).await {
                        Ok(task) => {
                            // Misma ventana de submit remoto abandonado que
                            // en copy/move (deuda #74).
                            cancellers.borrow_mut().push(task.canceller());
                            finish(&lua, &task.join().await)
                        }
                        Err(e) => err_mv(&lua, error_key(&e)),
                    }
                }
            })?,
        )?;
    }
    norte.set("fs", fs)?;

    // --- norte.pane: snapshot congelado, funciones síncronas ------------
    let pane = lua.create_table()?;
    {
        let wire = ctx.cwd.to_wire();
        pane.set(
            "cwd",
            lua.create_function(move |lua, ()| lua.create_string(wire.as_bytes()))?,
        )?;
    }
    {
        let wire = ctx.other_cwd.to_wire();
        pane.set(
            "other_cwd",
            lua.create_function(move |lua, ()| lua.create_string(wire.as_bytes()))?,
        )?;
    }
    {
        let wires: Vec<String> = ctx.selection.iter().map(VPath::to_wire).collect();
        pane.set(
            "selection",
            lua.create_function(move |lua, ()| {
                let t = lua.create_table()?;
                for (i, w) in wires.iter().enumerate() {
                    t.set(i + 1, lua.create_string(w.as_bytes())?)?;
                }
                Ok(t)
            })?,
        )?;
    }
    {
        // Último uso de `ctx`: `current` se consume.
        let wire = ctx.current.map(|p| p.to_wire());
        pane.set(
            "current",
            lua.create_function(move |lua, ()| match &wire {
                Some(w) => Ok(Value::String(lua.create_string(w.as_bytes())?)),
                None => Ok(Value::Nil),
            })?,
        )?;
    }
    norte.set("pane", pane)?;

    // --- norte.ui.message: acumula, el driver vuelca --------------------
    let ui: mlua::Table = norte.get("ui")?;
    ui.set(
        "message",
        lua.create_function(move |_, s: mlua::String| {
            let mut msgs = messages.borrow_mut();
            if msgs.len() < MESSAGES_MAX {
                msgs.push(String::from_utf8_lossy(&s.as_bytes()).into_owned());
            } else if let Some(last) = msgs.last_mut() {
                // Tope alcanzado: se descarta y se marca el desbordamiento
                // (ver MESSAGES_MAX).
                "…".clone_into(last);
            }
            Ok(())
        })?,
    )?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire")
    }

    /// Absoluto UTF-8 = interpretación wire (percent-decoding).
    #[test]
    fn absoluto_wire_percent_decodifica() {
        let base = vp("mem:///");
        let p = to_vpath(&base, b"mem:///%FF%FE").expect("wire");
        assert_eq!(p.file_name().unwrap().as_bytes(), &[0xFF, 0xFE]);
    }

    /// Absoluto NO-UTF8 (concatenación cruda en Lua) = segmentos crudos.
    #[test]
    fn absoluto_crudo_no_decodifica() {
        let base = vp("mem:///");
        let p = to_vpath(&base, b"mem:///\xFF\xFE").expect("crudo");
        assert_eq!(p.file_name().unwrap().as_bytes(), &[0xFF, 0xFE]);
        assert_eq!(p.to_wire(), "mem:///%FF%FE");
    }

    /// Relativo = crudo colgado de la base, multi-segmento incluido.
    #[test]
    fn relativo_cuelga_de_la_base_en_crudo() {
        let base = vp("mem:///d");
        let p = to_vpath(&base, b"sub/100%").expect("relativo");
        assert_eq!(p.to_wire(), "mem:///d/sub/100%25");
    }

    /// Inválidos: vacío, segmento vacío, `..`, NUL.
    #[test]
    fn invalidos_dan_clave_estable() {
        let base = vp("mem:///");
        for raw in [&b""[..], b"a//b", b"..", b"a\x00b", b"mem://\xFF/x"] {
            assert_eq!(
                to_vpath(&base, raw),
                Err(error_key(&Error::InvalidPath)),
                "{}",
                String::from_utf8_lossy(raw)
            );
        }
    }

    /// `norte.ui.message` tiene tope: un script en bucle no crece la cola
    /// sin límite; el desbordamiento queda MARCADO (último = "…").
    #[test]
    fn message_tiene_tope_y_marca_desbordamiento() {
        let lua = Lua::new();
        let norte = lua.create_table().unwrap();
        norte.set("ui", lua.create_table().unwrap()).unwrap();
        lua.globals().set("norte", norte).unwrap();

        let backend = Backend::Embedded(std::sync::Arc::new(norte_core::Engine::new()));
        let ctx = PaneCtx {
            cwd: vp("mem:///"),
            other_cwd: vp("mem:///"),
            selection: vec![],
            current: None,
        };
        let messages: Rc<RefCell<Vec<String>>> = Rc::default();
        install_fs(
            &lua,
            backend,
            ctx,
            Rc::default(),
            Rc::clone(&messages),
            Rc::default(),
        )
        .unwrap();

        lua.load("for i = 1, 100 do norte.ui.message('m' .. i) end")
            .exec()
            .unwrap();
        let msgs = messages.borrow();
        assert_eq!(msgs.len(), MESSAGES_MAX, "jamás por encima del tope");
        assert_eq!(msgs.last().unwrap(), "…", "desbordamiento marcado");
        assert_eq!(msgs[0], "m1", "los primeros se conservan");
    }
}
