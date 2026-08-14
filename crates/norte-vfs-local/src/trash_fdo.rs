//! Papelera freedesktop.org implementada AQUÍ, sin delegar en el crate
//! `trash` (Linux/BSD; macOS y Windows siguen delegando).
//!
//! # Por qué existe este módulo
//! El crate `trash` sabe enterrar un fichero pero no dice DÓNDE lo puso, así
//! que [`Provider::trash`](norte_vfs::Provider::trash) contestaba `Ok(None)` y
//! el journal se quedaba sin `reversal_ref`. El undo de una sobrescritura
//! tenía entonces que casar por ruta ORIGINAL y quedarse con el ítem más
//! reciente — que para cuando llega ahí es el fichero que el propio undo acaba
//! de enterrar al deshacer la mitad `created` de la pareja. Restauraba el
//! fichero NUEVO sobre sí mismo, dejaba el original del usuario dentro de la
//! papelera y lo contaba como éxito (BLOCKER de la review de seguridad de la
//! tarea 11 del plan de sincronización).
//!
//! La spec de freedesktop es corta y el destino lo elegimos nosotros: **si lo
//! elegimos, lo sabemos**. Es la misma forma que ya tiene la papelera LÓGICA
//! de `norte-vfs-sftp` (`.norte-trash/<id>/payload`).
//!
//! # Reglas duras que dirigen el diseño
//! - **Regla 1 (los nombres son BYTES).** El fichero conserva sus bytes tal
//!   cual dentro de `files/`; el sidecar `info/<nombre>.trashinfo` es TEXTO y
//!   guarda la ruta original percent-codificada, con lo que sale ASCII puro
//!   pase lo que pase por delante. Los dos no pueden discrepar porque no
//!   nombran lo mismo: el sidecar nombra el ORIGEN (que es lo que el restore
//!   necesita) y el fichero se llama como puede (deduplicado, y truncado si el
//!   `NAME_MAX` aprieta). Aquí no se normaliza NADA: ni NFC, ni NFD, ni cajas.
//! - **Regla 2 (nada de I/O bloqueante en async).** Todo este módulo es
//!   síncrono y lo llama el provider desde `blocking()`.
//! - **Regla 5 (`unsafe` justificado).** Solo `getuid` y `localtime_r`, las
//!   dos con su `// SAFETY:`.
//!
//! # El orden importa, y la spec dice cuál
//! Primero el sidecar con `O_EXCL` —eso es lo que hace ATÓMICA la reserva del
//! nombre frente a otro proceso—, después el movimiento de la víctima. Si el
//! movimiento falla, se desenlaza el sidecar y el nombre vuelve a estar libre.
//!
//! La reserva cubre `info/<nombre>.trashinfo`, no `files/<nombre>`: lo segundo
//! lo cubre el `rename` no-replace, y ahí queda una rendija en los FS sin
//! `renameat2(RENAME_NOREPLACE)` (NFS, y los unix que no son Linux ni macOS),
//! donde `do_rename` degrada a comprobar-y-renombrar. Otra papelera que
//! escribiera su `files/x` dentro de esa ventana perdería su fichero. Las
//! implementaciones que se respetan reservan el `info/` primero igual que
//! nosotros, así que la ventana pide una que no lo haga Y un FS sin la
//! primitiva (MINOR del security-reviewer).

use std::ffi::OsStr;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use norte_proto::{ConflictKind, Error};
use norte_vfs::trash::TrashId;

use crate::provider::{do_rename, map_io};

/// Subdirectorio de los ficheros enterrados.
pub(crate) const FILES: &str = "files";
/// Subdirectorio de los sidecars de metadatos.
pub(crate) const INFO: &str = "info";
/// Sufijo del sidecar, que la spec fija.
const INFO_SUFFIX: &[u8] = b".trashinfo";
/// Tope de un nombre de entrada en un FS unix corriente. El sidecar añade
/// [`INFO_SUFFIX`], así que el nombre de la entrada se recorta para que quepan
/// los dos.
///
/// Es un PUNTO DE PARTIDA, no un hecho: eCryptfs corta sobre los 143 bytes,
/// gocryptfs sobre los 175, y un servidor NFS o CIFS pone el suyo. Por eso el
/// bucle de [`trash`] recorta y reintenta cuando el FS rechaza el nombre en vez
/// de rendirse (hallazgo MAJOR-1 del encoding-auditor).
const NAME_MAX: usize = 255;
/// Suelo del recorte: por debajo, el nombre deja de parecerse a nada y más vale
/// fallar limpio.
const MIN_NAME_BUDGET: usize = 16;
/// Cuántos nombres `x`, `x.2`… `x.8` se prueban antes de pasar al nombre
/// derivado del `id`.
///
/// El tope no es estético. Con una escala abierta (`x.2`, `x.3`, … `x.4096`),
/// sembrar cuatro mil sidecars vacíos deja un nombre INTRASHEABLE para siempre,
/// y un `Mirror` de un árbol con miles de `index.html` cuesta un `open()` por
/// hueco. Tras ocho sondas se salta al hueco `x.<id>`, que es único por
/// operación y se acierta a la primera.
const PROBES: u32 = 8;
/// Cuántas veces se recorta el nombre ante un rechazo del FS antes de rendirse.
const MAX_SHRINKS: u32 = 4;

/// Entierra `victim` en la papelera que le corresponda y devuelve la ruta
/// NATIVA exacta de `files/<nombre>` — el destino recuperable que el journal
/// guarda como `reversal_ref`.
///
/// `data_home` sustituye a `$XDG_DATA_HOME` cuando viene (costura de test:
/// ver [`LocalProvider::with_trash_home`](crate::LocalProvider::with_trash_home)).
///
/// # Idempotencia (#99)
/// `id` NO se inventa aquí: lo genera el engine una vez por operación y de él
/// sale la `DeletionDate` del sidecar, de modo que un reintento escribe el
/// MISMO sidecar byte a byte. Hay dos caminos, y los dos convergen:
///
/// - **El intento anterior falló al mover.** Deshizo su reserva, así que el
///   reintento vuelve a elegir el MISMO nombre libre y el mismo destino.
/// - **El intento anterior movió, pero su respuesta se perdió.** La víctima ya
///   no está; el reintento reconoce su propia entrada —mismo `Path=` y misma
///   `DeletionDate=`— y devuelve su destino en vez de crear una segunda o
///   perder el `reversal_ref`.
///
/// Residuo honesto, y conviene leerlo entero. Una entrada se identifica por
/// (ruta original, instante de borrado), que es lo único que cabe en un
/// `.trashinfo` estándar — y ese instante tiene resolución de SEGUNDO. Dos
/// operaciones que compartan las dos cosas —la misma ruta y el mismo segundo—
/// son indistinguibles, y si la segunda encuentra la ruta ya vacía heredaría el
/// destino de la primera. Distinguirlas del todo pediría meter una clave
/// privada en la papelera del usuario, que otras papeleras leen.
///
/// Lo que ACOTA ese residuo no es el `.trashinfo` sino el directorio: la
/// papelera es 0700 y de este uid ([`ensure_dir_owned`]), y tanto el sidecar
/// como el payload se comprueban propios ([`is_ours`]), así que la entrada que
/// se hereda salió de este usuario y de esa misma ruta. Lo que este módulo NO
/// puede prometer es que el CONTENIDO no haya cambiado entre los dos intentos:
/// otro proceso de este mismo usuario puede haber sustituido el payload, y eso
/// queda fuera del modelo de amenazas (`SECURITY.md`).
///
/// # Errors
/// [`Error::NotFound`] si la víctima no está (y no hay entrada previa nuestra);
/// [`Error::Unsupported`] si no hay ninguna papelera utilizable para ese punto
/// de montaje (el frontend reofrece borrado permanente, ADR 0009);
/// [`Error::Conflict`] si todos los huecos estaban ocupados.
pub(crate) fn trash(
    victim: &Path,
    data_home: Option<&Path>,
    id: &TrashId,
) -> Result<PathBuf, Error> {
    let dir = prepare_trash_dir(victim, data_home)?;
    let name = victim
        .file_name()
        .ok_or(Error::InvalidPath)?
        .as_bytes()
        .to_vec();
    let body = trashinfo(&original_path(victim)?, id.deleted_ms());

    // La víctima ya no está: o nunca existió, o la movió un intento anterior de
    // ESTA misma operación. Lo segundo se reconoce por el sidecar.
    if std::fs::symlink_metadata(victim).is_err() {
        return converged(&dir, &name, &body, id).ok_or(Error::NotFound);
    }

    let mut budget = NAME_MAX - INFO_SUFFIX.len();
    let mut shrinks = 0;
    let mut k = 1;
    while k <= PROBES + 1 {
        let cand = candidate(&name, k, budget, id);
        let sidecar = dir.join(INFO).join(join_bytes(&cand, INFO_SUFFIX));
        let dest = dir.join(FILES).join(OsStr::from_bytes(&cand));
        match write_new(&sidecar, &body) {
            Ok(()) => match do_rename(victim, &dest) {
                Ok(()) => return Ok(dest),
                // El nombre estaba reservado en `info/` pero OCUPADO en
                // `files/` (una entrada a medio escribir, o algo sembrado):
                // suelta la reserva y prueba el siguiente.
                Err(Error::Conflict { .. }) => {
                    let _ = std::fs::remove_file(&sidecar);
                    k += 1;
                }
                // Cualquier otro fallo deja el nombre libre: el sidecar sin
                // fichero sería una entrada fantasma en la papelera del usuario.
                Err(e) => {
                    let _ = std::fs::remove_file(&sidecar);
                    return Err(e);
                }
            },
            // El nombre está reservado por alguien: el siguiente. Aquí NO se
            // converge aunque el sidecar sea idéntico al nuestro — la víctima
            // está delante, así que hay algo que mover; reclamar la entrada de
            // un intento anterior dejaría el fichero en su sitio y lo contaría
            // como enterrado.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => k += 1,
            // **El FS rechaza el NOMBRE, no la operación.** `ENAMETOOLONG` en
            // un `$HOME` sobre eCryptfs (tope ~143), `EINVAL`/`EILSEQ` en un
            // vfat montado `utf8=1` o un ext4 con `casefold` si el recorte
            // partió un carácter. El nombre de la entrada es ALMACENAMIENTO
            // —la ruta buena vive en el sidecar—, así que se recorta y se
            // reintenta el mismo hueco en vez de declarar intrasheable un
            // fichero que el propio volumen aceptó (MAJOR-1 del
            // encoding-auditor).
            Err(e) if name_rejected(&e) && shrinks < MAX_SHRINKS && budget > MIN_NAME_BUDGET => {
                budget = (budget / 2).max(MIN_NAME_BUDGET);
                shrinks += 1;
            }
            Err(e) => return Err(map_io(&e)),
        }
    }
    Err(Error::Conflict {
        conflict: ConflictKind::Exists,
    })
}

/// ¿Este error dice «ese NOMBRE no me vale» en vez de «esa operación no se
/// puede»? `ENAMETOOLONG`, `EINVAL` y `EILSEQ` — los tres los contesta el FS
/// mirando los bytes del nombre, y los tres se arreglan acortándolo.
fn name_rejected(e: &std::io::Error) -> bool {
    matches!(
        e.kind(),
        std::io::ErrorKind::InvalidFilename | std::io::ErrorKind::InvalidInput
    ) || matches!(
        e.raw_os_error(),
        Some(libc::ENAMETOOLONG | libc::EILSEQ | libc::EINVAL)
    )
}

/// Olvida el sidecar de `dest` tras restaurarlo (best-effort).
///
/// Se llama DESPUÉS de mover el fichero de vuelta, nunca antes: un `files/x`
/// sin su `info/x.trashinfo` es una entrada que ninguna papelera gráfica
/// enseña, así que perder el sidecar primero y fallar el movimiento después
/// escondería el fichero. Al revés lo peor que queda es un sidecar huérfano.
pub(crate) fn forget_sidecar(dest: &Path) {
    let Some(sidecar) = sidecar_of(dest) else {
        return;
    };
    // La FORMA de la ruta no basta para borrar: `sidecar_of` acepta cualquier
    // cosa cuyo padre se llame `files`, así que un `reversal_ref` manipulado
    // —o simplemente un proyecto del usuario con un `files/` y un `info/` al
    // lado— apuntaría a un fichero suyo. Se exige que sea un fichero REGULAR y
    // que empiece por la cabecera de la spec: entonces es un `.trashinfo`, y
    // borrarlo es lo que toca (MINOR-5 del encoding-auditor, MINOR del
    // security-reviewer).
    let regular = std::fs::symlink_metadata(&sidecar).is_ok_and(|md| md.file_type().is_file());
    if regular && std::fs::read(&sidecar).is_ok_and(|b| b.starts_with(HEADER.as_bytes())) {
        let _ = std::fs::remove_file(&sidecar);
    }
}

/// El sidecar que le toca a `<papelera>/files/<nombre>`, o `None` si `dest` no
/// tiene esa forma (no salió de esta papelera).
fn sidecar_of(dest: &Path) -> Option<PathBuf> {
    let name = dest.file_name()?;
    let files = dest.parent()?;
    if files.file_name() != Some(OsStr::new(FILES)) {
        return None;
    }
    Some(
        files
            .parent()?
            .join(INFO)
            .join(join_bytes(name.as_bytes(), INFO_SUFFIX)),
    )
}

/// La entrada que un intento anterior dejó, si la dejó.
///
/// Se recorren TODOS los huecos que [`trash`] podría haber usado, incluidos los
/// recortes: parar en el primero cuyo sidecar falta sería suponer que los huecos
/// se llenan de abajo arriba, y este módulo hace agujeros él solo —el intento
/// que reserva `x` y falla el rename suelta `x` y se va a `x.2`, y
/// [`forget_sidecar`] vacía un hueco cualquiera al restaurar—. Con esa
/// suposición, un reintento no encontraba su propia entrada y contestaba
/// `NotFound` sobre un fichero YA enterrado, perdiendo el `reversal_ref`: el
/// bug que este módulo existe para no tener (MAJOR-2 del encoding-auditor).
///
/// El recorrido está acotado por construcción ([`PROBES`] × [`MAX_SHRINKS`]) y
/// solo corre cuando la víctima ya no está, que es el camino raro.
fn converged(dir: &Path, name: &[u8], body: &[u8], id: &TrashId) -> Option<PathBuf> {
    let mut budget = NAME_MAX - INFO_SUFFIX.len();
    for _ in 0..=MAX_SHRINKS {
        for k in 1..=PROBES + 1 {
            let cand = candidate(name, k, budget, id);
            let sidecar = dir.join(INFO).join(join_bytes(&cand, INFO_SUFFIX));
            let dest = dir.join(FILES).join(OsStr::from_bytes(&cand));
            if is_ours(&sidecar, &dest, body) {
                return Some(dest);
            }
        }
        if budget <= MIN_NAME_BUDGET {
            break;
        }
        budget = (budget / 2).max(MIN_NAME_BUDGET);
    }
    None
}

/// ¿El sidecar es EXACTAMENTE el que escribiríamos nosotros y el fichero está
/// donde dice? La comparación es byte a byte del cuerpo entero: misma ruta
/// original y misma `DeletionDate`, que sale del `id` del engine.
///
/// Lo que eso distingue y lo que no está en el residuo de [`trash`]: dos
/// entierros de la MISMA ruta en el MISMO segundo escriben el mismo cuerpo y
/// son indistinguibles. Cualquier otra entrada de la papelera —otra ruta, otro
/// segundo, otra aplicación— no cuela.
fn is_ours(sidecar: &Path, dest: &Path, body: &[u8]) -> bool {
    // NUESTROS los dos: el sidecar y el payload. La papelera es 0700 y de este
    // uid (lo impone `ensure_dir_owned`), así que esto es cinturón sobre
    // tirantes — pero es el cinturón que impide que una entrada sembrada se
    // haga pasar por la nuestra y acabe de `reversal_ref` en el journal
    // (MAJOR del security-reviewer).
    let mio = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|md| md.uid() == uid());
    mio(sidecar) && mio(dest) && std::fs::read(sidecar).is_ok_and(|found| found == body)
}

/// El candidato `k`-ésimo con el presupuesto de bytes que le quede al nombre.
///
/// `k` de 1 a [`PROBES`] es la deduplicación que la spec describe (`x`, `x.2`,
/// `x.3`…). El hueco [`PROBES`]`+1` es `x.<id>`, único por operación: cierra la
/// escala sin dejarla abierta a que otro la llene (ver [`PROBES`]).
///
/// El nombre se recorta si no cabe. Recortar no pierde nada —la ruta buena vive
/// en el sidecar, y el de `files/` es solo almacenamiento—, pero se recorta por
/// FRONTERA DE CARÁCTER cuando el nombre es UTF-8 válido: partir un carácter
/// produce un nombre que un vfat montado `utf8=1` o un ext4 con `casefold`
/// RECHAZAN, y entonces un fichero que el volumen aceptó se vuelve intrasheable.
/// Un nombre que nunca fue UTF-8 se corta por bytes, que es lo coherente: ese
/// volumen ya lo aceptaba así.
fn candidate(name: &[u8], k: u32, budget: usize, id: &TrashId) -> Vec<u8> {
    let suffix = match k {
        1 => Vec::new(),
        k if k <= PROBES => format!(".{k}").into_bytes(),
        _ => format!(".{}", id.as_segment()).into_bytes(),
    };
    let room = budget.saturating_sub(suffix.len()).max(1);
    let mut out = truncate_bytes(name, room).to_vec();
    // Un recorte no puede dejar un nombre que el FS no acepta como entrada.
    if out.is_empty() || out == b"." || out == b".." {
        out = b"trashed".to_vec();
    }
    out.extend_from_slice(&suffix);
    out
}

/// `name` recortado a `room` bytes, por frontera de carácter si es UTF-8.
fn truncate_bytes(name: &[u8], room: usize) -> &[u8] {
    if name.len() <= room {
        return name;
    }
    match std::str::from_utf8(name) {
        Ok(texto) => {
            let mut fin = room;
            while fin > 0 && !texto.is_char_boundary(fin) {
                fin -= 1;
            }
            &name[..fin]
        }
        Err(_) => &name[..room],
    }
}

/// `a` seguido de `b` como nombre de fichero, sin pasar por `str` (regla 1).
fn join_bytes(a: &[u8], b: &[u8]) -> std::ffi::OsString {
    let mut out = a.to_vec();
    out.extend_from_slice(b);
    std::ffi::OsString::from_vec(out)
}

/// Crea el fichero con `O_EXCL` y le escribe `body`.
///
/// `create_new` ES `O_EXCL|O_CREAT`: no sigue symlinks y falla si el nombre
/// existe, que es justo lo que convierte la reserva en atómica frente a otro
/// proceso escribiendo en la misma papelera.
fn write_new(path: &Path, body: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    // Si el CONTENIDO no llega (ENOSPC, EIO), el nombre ya está creado: hay que
    // soltarlo. Sin esto queda un `.trashinfo` vacío, que es una entrada
    // fantasma en la papelera del usuario y un hueco gastado para siempre
    // (MINOR del security-reviewer).
    if let Err(e) = f.write_all(body).and_then(|()| f.sync_all()) {
        drop(f);
        let _ = std::fs::remove_file(path);
        return Err(e);
    }
    Ok(())
}

/// Cabecera de sección del `.trashinfo`, que la spec fija.
const HEADER: &str = "[Trash Info]\n";

/// El cuerpo del `.trashinfo`: cabecera, ruta original percent-codificada y
/// fecha de borrado en hora LOCAL, que es lo que la spec pide.
fn trashinfo(original: &[u8], deleted_ms: u64) -> Vec<u8> {
    format!(
        "{HEADER}Path={}\nDeletionDate={}\n",
        percent_encode(original),
        local_datetime(deleted_ms)
    )
    .into_bytes()
}

/// La ruta ORIGINAL que va al sidecar: el parent CANONICALIZADO más el nombre
/// tal cual.
///
/// Se canoniza el parent y no la víctima porque la víctima puede ser un
/// symlink y lo que se entierra es el symlink, no su destino. Y se canoniza
/// porque es lo que guardan las demás implementaciones —y lo que
/// `restore_trashed` (que lista la papelera con el crate `trash`) espera casar—:
/// una raíz colgada de un symlink no acertaría de otro modo.
///
/// # Errors
/// [`Error`] del OS si el parent no se puede canonizar.
fn original_path(victim: &Path) -> Result<Vec<u8>, Error> {
    let name = victim.file_name().ok_or(Error::InvalidPath)?;
    let parent = victim.parent().ok_or(Error::InvalidPath)?;
    let real = parent.canonicalize().map_err(|e| map_io(&e))?;
    Ok(real.join(name).into_os_string().into_vec())
}

/// Percent-codifica BYTES según la spec del `.trashinfo` (RFC 2396 sobre los
/// bytes crudos, dejando `/` legible).
///
/// Deja sin escapar solo el conjunto `unreserved` de RFC 3986 más `/`. Escapa
/// de más respecto a lo que hace glib (`!~*'()` los deja pasar), y eso es
/// deliberado: decodifica idéntico en cualquier lector y no hay que razonar
/// sobre qué carácter es especial en qué contexto. Lo que importa es que
/// **cualquier byte que pudiera romper el formato clave=valor sale escapado**:
/// un `\n` en un nombre de fichero se convierte en `%0A` y no puede inyectar
/// una línea `Path=` falsa.
///
/// El resultado es ASCII puro, así que el sidecar es UTF-8 válido incluso
/// cuando el nombre del fichero no lo es (regla 1: el nombre no se decodifica
/// jamás, se codifica byte a byte).
fn percent_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'/') {
            out.push(char::from(b));
        } else {
            out.push('%');
            out.push(char::from(HEX[usize::from(b >> 4)]));
            out.push(char::from(HEX[usize::from(b & 0x0f)]));
        }
    }
    out
}

/// `YYYY-MM-DDThh:mm:ss` en hora LOCAL desde un instante en milisegundos.
///
/// Hora local porque es lo que la spec pide y lo que enseñan las papeleras
/// gráficas. Sale de `localtime_r`, que es la única forma de aplicar la zona
/// del sistema sin meter una dependencia de calendario (regla 8) — la
/// alternativa sería escribir UTC y que la papelera del usuario mintiera por
/// el desfase de su zona.
#[allow(unsafe_code)]
fn local_datetime(deleted_ms: u64) -> String {
    const FALLBACK: &str = "1970-01-01T00:00:00";
    let Ok(secs) = libc::time_t::try_from(deleted_ms / 1000) else {
        return FALLBACK.to_owned();
    };
    // SAFETY: `libc::tm` es POD (enteros más un `*const c_char` para el nombre
    // de la zona, y el puntero nulo es un valor válido), así que el patrón
    // todo-ceros es una instancia válida.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: `localtime_r` escribe en el `tm` que se le pasa y no guarda
    // ninguno de los dos punteros; ambos apuntan a locales vivas durante toda
    // la llamada y están correctamente alineados. Es la variante REENTRANTE:
    // no toca estado global compartido. Contrato testeado en
    // `tests::la_fecha_de_borrado_es_iso_local`.
    let ok = unsafe { libc::localtime_r(std::ptr::from_ref(&secs), std::ptr::from_mut(&mut tm)) };
    if ok.is_null() {
        return FALLBACK.to_owned();
    }
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}",
        tm.tm_year + 1900,
        tm.tm_mon + 1,
        tm.tm_mday,
        tm.tm_hour,
        tm.tm_min,
        tm.tm_sec
    )
}

/// Deja lista la papelera que le toca a `victim` y devuelve su raíz.
///
/// La papelera "home" (`$XDG_DATA_HOME/Trash`) solo sirve si está en el MISMO
/// dispositivo que la víctima; si no, la spec manda usar la del punto de
/// montaje de la víctima. Eso no es un detalle de rendimiento: papelerizar
/// entre dispositivos sería un copy+delete largo e incancelable a mitad
/// (excepción de plataforma que ADR 0009 anotaba y que este camino ya no
/// tiene: aquí el movimiento es SIEMPRE un `rename` dentro de un dispositivo).
/// Es el issue #26, y lo fija
/// `tests::la_papelera_jamas_cruza_de_dispositivo` con dos dispositivos de
/// verdad — sin un test, «ya no copia» es una frase, no una garantía.
///
/// # Las dos papeleras NO se validan igual, y es deliberado
/// La de casa cuelga de `$HOME`: quien pueda escribir ahí ya es este usuario, y
/// un `~/.local/share/Trash` que es un symlink a otro disco es una decisión
/// suya que se respeta (glib hace lo mismo). La del TOPDIR cuelga de la raíz de
/// un montaje que puede ser compartida y escribible por otros, y ahí se exige
/// que el directorio sea NUESTRO —lstat, no stat, y `st_uid` propio— antes de
/// meter nada dentro. Sin esa comprobación, sembrar `/tmp/.Trash-1000` (mode
/// 0777, de otro uid) basta para que los ficheros que este usuario tira acaben
/// en un árbol ajeno, y para que el `reversal_ref` del journal apunte a un sitio
/// donde otro puede cambiar el contenido antes del undo — BLOCKER del
/// security-reviewer.
///
/// # Errors
/// [`Error::Unsupported`] si no hay ninguna papelera utilizable —incluido el
/// caso de que la del topdir exista y no sea nuestra—. El frontend reofrece
/// entonces el borrado PERMANENTE con aviso (ADR 0009).
fn prepare_trash_dir(victim: &Path, data_home: Option<&Path>) -> Result<PathBuf, Error> {
    let parent = victim.parent().ok_or(Error::InvalidPath)?;
    let dev = std::fs::metadata(parent).map_err(|e| map_io(&e))?.dev();
    if let Some(home) = home_trash(data_home)
        && device_of_nearest(&home) == Some(dev)
    {
        ensure_home_layout(&home)?;
        return Ok(home);
    }
    let top = top_dir(parent).ok_or(Error::Unsupported)?;
    let uid = uid();
    // `$top/.Trash/$uid` primero (spec), y si no vale se cae a `$top/.Trash-$uid`
    // en vez de rendirse: un `$uid` sembrado por otro dentro del `.Trash`
    // compartido no puede dejar sin papelera a nadie (glib se comporta igual).
    if let Some(compartida) = shared_topdir_trash(&top, uid)
        && ensure_owned_layout(&compartida).is_ok()
    {
        return Ok(compartida);
    }
    let propia = top.join(format!(".Trash-{uid}"));
    ensure_owned_layout(&propia)?;
    Ok(propia)
}

/// Crea `<papelera>/`, `files/` e `info/` bajo `$HOME` con permisos `0700`.
///
/// `0700` porque el contenido de una papelera es del usuario y de nadie más:
/// los nombres de lo que ha borrado ya son información.
///
/// # Errors
/// [`Error`] del OS si alguno no se puede crear (papelera en un montaje de
/// solo lectura, por ejemplo).
fn ensure_home_layout(dir: &Path) -> Result<(), Error> {
    if let Some(parent) = dir.parent() {
        // El contenedor (`~/.local/share`) con los permisos que le toquen: solo
        // la papelera es 0700.
        std::fs::create_dir_all(parent).map_err(|e| map_io(&e))?;
    }
    for d in [dir.to_path_buf(), dir.join(FILES), dir.join(INFO)] {
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(&d)
            .map_err(|e| map_io(&e))?;
    }
    Ok(())
}

/// Lo mismo en un TOPDIR, donde el directorio tiene que ser NUESTRO.
///
/// # Errors
/// [`Error::Unsupported`] si alguno de los tres no se puede crear o no pasa la
/// comprobación de propiedad: "aquí no hay papelera utilizable", que es
/// exactamente lo que ADR 0009 hace que el frontend sepa manejar. No se
/// distingue un `EROFS` de un `.Trash-$uid` ajeno a propósito — para quien
/// borra son la misma respuesta, y el errno de un montaje que no controlamos no
/// es información que el frontend pueda usar.
fn ensure_owned_layout(dir: &Path) -> Result<(), Error> {
    ensure_dir_owned(dir)?;
    ensure_dir_owned(&dir.join(FILES))?;
    ensure_dir_owned(&dir.join(INFO))
}

/// Crea `dir` con `0700` si falta, y en cualquier caso comprueba que es un
/// DIRECTORIO de verdad (no un symlink) y NUESTRO.
///
/// `create_dir` sin `recursive`: la versión recursiva, ante un `EEXIST`,
/// pregunta con `metadata`, que SIGUE symlinks — un symlink a un árbol ajeno
/// pasaría por directorio válido. Aquí se comprueba con `symlink_metadata`, que
/// no sigue nada, y se exige `st_uid` propio: quien no es dueño no puede
/// haberlo hecho nuestro (no hay `chown` hacia otro uid sin privilegios).
///
/// Si es nuestro pero con permisos flojos, se APRIETA a `0700` en vez de
/// rechazarlo: los nombres de lo que uno borra no son de nadie más, y el mismo
/// criterio usa el directorio de spool del core.
fn ensure_dir_owned(dir: &Path) -> Result<(), Error> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(Error::Unsupported),
    }
    let md = std::fs::symlink_metadata(dir).map_err(|_| Error::Unsupported)?;
    if !md.file_type().is_dir() || md.uid() != uid() {
        return Err(Error::Unsupported);
    }
    if md.permissions().mode() & 0o077 != 0 {
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
            .map_err(|_| Error::Unsupported)?;
    }
    Ok(())
}

/// `$XDG_DATA_HOME/Trash`, con `$HOME/.local/share/Trash` de respaldo.
///
/// Un `$XDG_DATA_HOME` RELATIVO se ignora, como manda la spec XDG: una raíz de
/// papelera relativa al cwd del daemon no es una raíz.
pub(crate) fn home_trash(data_home: Option<&Path>) -> Option<PathBuf> {
    let base = match data_home {
        Some(d) => d.to_path_buf(),
        None => match std::env::var_os("XDG_DATA_HOME").map(PathBuf::from) {
            Some(d) if d.is_absolute() => d,
            _ => PathBuf::from(std::env::var_os("HOME")?)
                .join(".local")
                .join("share"),
        },
    };
    base.is_absolute().then(|| base.join("Trash"))
}

/// `st_dev` del ancestro EXISTENTE más cercano a `p`.
///
/// Hace falta porque la papelera "home" puede no existir todavía y hay que
/// decidir su dispositivo ANTES de crearla — crearla para averiguarlo dejaría
/// un directorio en un sitio donde a lo mejor no se va a enterrar nada.
fn device_of_nearest(p: &Path) -> Option<u64> {
    let mut cur = p;
    loop {
        if let Ok(md) = std::fs::metadata(cur) {
            return Some(md.dev());
        }
        cur = cur.parent()?;
    }
}

/// El punto de montaje de `p`: el ancestro más alto con su mismo `st_dev`.
/// Sin leer `/proc/mounts` — que además no es UTF-8 por contrato y ha hecho
/// panicar al crate `trash` upstream.
fn top_dir(p: &Path) -> Option<PathBuf> {
    let dev = std::fs::metadata(p).ok()?.dev();
    let mut top = p.to_path_buf();
    while let Some(parent) = top.parent() {
        match std::fs::metadata(parent) {
            Ok(md) if md.dev() == dev => top = parent.to_path_buf(),
            _ => return Some(top),
        }
    }
    Some(top)
}

/// `$top/.Trash/$uid` cuando `$top/.Trash` existe y la spec la da por buena:
/// un DIRECTORIO, con el bit sticky y que no sea un symlink. `None` si no.
///
/// `symlink_metadata` y no `metadata`: la spec exige explícitamente que
/// `$top/.Trash` no sea un symlink, y con esta llamada un symlink no es un
/// directorio. Es la comprobación que impide que quien pueda escribir en la
/// raíz de un montaje compartido redirija las papeleras de los demás a un árbol
/// suyo. El sticky es la otra mitad: sin él, cualquiera borra lo de cualquiera.
///
/// Que el `$uid` de dentro sea NUESTRO lo comprueba [`ensure_dir_owned`]; el
/// sticky de la carpeta madre no lo garantiza, porque sembrar una entrada nueva
/// con el nombre de otro uid sí es legal en un directorio sticky.
fn shared_topdir_trash(top: &Path, uid: u32) -> Option<PathBuf> {
    let shared = top.join(".Trash");
    let md = std::fs::symlink_metadata(&shared).ok()?;
    (md.file_type().is_dir() && md.permissions().mode() & 0o1000 != 0)
        .then(|| shared.join(uid.to_string()))
}

/// El uid real del proceso, que es el que nombra la papelera de un topdir.
#[allow(unsafe_code)]
fn uid() -> u32 {
    // SAFETY: `getuid` no recibe punteros, no puede fallar y no toca estado
    // que este proceso comparta; su ABI la fija POSIX.
    unsafe { libc::getuid() }
}

#[cfg(test)]
mod tests {
    use super::{
        candidate, local_datetime, percent_encode, shared_topdir_trash, sidecar_of, trashinfo,
    };
    use norte_vfs::trash::TrashId;
    use std::path::Path;

    /// #26: la papelera JAMÁS cruza una frontera de dispositivo.
    ///
    /// El fallo que este test fija no es hipotético: el crate `trash`, que es
    /// quien hacía esto antes de que existiera este módulo, degrada a
    /// copiar-el-árbol-y-borrar-el-origen cuando el montaje de la víctima no
    /// admite papelera. Eso son GB dentro de UN `spawn_blocking`, sin progreso
    /// y sin cancelación (regla dura 3), y el fichero deja de estar donde
    /// estaba antes de que nadie pueda parar nada.
    ///
    /// Aquí la papelera se elige POR DISPOSITIVO ([`super::prepare_trash_dir`])
    /// y el traslado es un `rename`, que a través de una frontera falla en vez
    /// de copiar. El test lo comprueba de verdad, con dos dispositivos reales:
    /// la víctima en `/dev/shm` (tmpfs) y una home-trash inyectada en el disco.
    ///
    /// Sin dos dispositivos no hay nada que comprobar y el test se retira
    /// diciéndolo: fingirlo sería peor que no correrlo.
    #[cfg(target_os = "linux")]
    #[test]
    fn la_papelera_jamas_cruza_de_dispositivo() {
        use std::os::unix::fs::MetadataExt;

        let casa = tempfile::tempdir().expect("tempdir");
        let Ok(shm) = std::fs::metadata("/dev/shm") else {
            eprintln!("sin /dev/shm: no hay dos dispositivos que comprobar");
            return;
        };
        if shm.dev() == std::fs::metadata(casa.path()).expect("stat").dev() {
            eprintln!("/dev/shm y el tempdir son el MISMO dispositivo: nada que comprobar");
            return;
        }
        let base = Path::new("/dev/shm").join(format!("norte-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).expect("dir en /dev/shm");
        let victima = base.join("v.txt");
        std::fs::write(&victima, b"x").expect("victima");

        let dest = super::trash(&victima, Some(casa.path()), &id());

        let veredicto = match &dest {
            Ok(dest) => {
                let d = std::fs::metadata(dest).expect("stat del destino").dev();
                assert_eq!(
                    d,
                    shm.dev(),
                    "la entrada se quedó en el dispositivo de la víctima"
                );
                assert!(
                    !casa
                        .path()
                        .join("Trash")
                        .join("files")
                        .join("v.txt")
                        .exists(),
                    "y NADA se copió a la papelera de $HOME"
                );
                let _ = std::fs::remove_file(dest);
                let _ = std::fs::remove_file(super::sidecar_of(dest).expect("sidecar"));
                // Y la papelera del topdir que este test acaba de crear, SOLO
                // si queda vacía: `remove_dir` falla con contenido, que es
                // exactamente la protección que hace falta para no barrer la
                // papelera de verdad de nadie.
                if let Some(papelera) = dest.parent().and_then(Path::parent) {
                    let _ = std::fs::remove_dir(papelera.join(super::FILES));
                    let _ = std::fs::remove_dir(papelera.join(super::INFO));
                    let _ = std::fs::remove_dir(papelera);
                }
                true
            }
            // `Unsupported` es la OTRA respuesta correcta (montaje sin
            // papelera utilizable): el frontend reofrece borrado permanente
            // con aviso, ADR 0009. Lo que no vale es copiar.
            Err(norte_proto::Error::Unsupported) => {
                assert!(victima.exists(), "una papelera imposible no mueve nada");
                true
            }
            Err(e) => panic!("ni papelera en el dispositivo ni Unsupported: {e:?}"),
        };
        let _ = std::fs::remove_dir_all(&base);
        assert!(veredicto);
    }

    fn id() -> TrashId {
        TrashId::new(1_726_000_000_123, 7)
    }

    /// El presupuesto de bytes con el que arranca el bucle de [`super::trash`].
    fn budget() -> usize {
        super::NAME_MAX - super::INFO_SUFFIX.len()
    }

    /// Regla 1: el sidecar es ASCII (y por tanto UTF-8 válido) aunque el nombre
    /// no lo sea, y ningún byte del nombre sobrevive sin escapar en una
    /// posición donde pudiera romper el formato.
    #[test]
    fn un_nombre_no_utf8_sale_escapado_y_el_sidecar_es_ascii() {
        let body = trashinfo(b"/tmp/x/h\xffstil\n=raro.bin", 0);
        let text = String::from_utf8(body).expect("el sidecar es UTF-8 por construcción");
        assert!(text.is_ascii(), "{text}");
        assert!(
            text.contains("Path=/tmp/x/h%FFstil%0A%3Draro.bin"),
            "{text}"
        );
        // Tres líneas exactas: la inyección de un `\n` no puede añadir una cuarta.
        assert_eq!(text.lines().count(), 3, "{text}");
        assert!(text.starts_with("[Trash Info]\n"));
    }

    #[test]
    fn el_percent_encoding_deja_legible_lo_que_no_estorba() {
        assert_eq!(percent_encode(b"/home/u/a-b_c.d~e"), "/home/u/a-b_c.d~e");
        assert_eq!(percent_encode(b" %#?"), "%20%25%23%3F");
        assert_eq!(percent_encode("café".as_bytes()), "caf%C3%A9");
    }

    /// La deduplicación de la spec, y el recorte que el `NAME_MAX` obliga: el
    /// nombre de la entrada más `.trashinfo` tiene que caber.
    #[test]
    fn los_candidatos_deduplican_y_caben() {
        assert_eq!(candidate(b"a.txt", 1, budget(), &id()), b"a.txt");
        assert_eq!(candidate(b"a.txt", 2, budget(), &id()), b"a.txt.2");
        assert_eq!(candidate(b"a.txt", 8, budget(), &id()), b"a.txt.8");
        // Pasadas las sondas, el hueco lo nombra el id: único por operación, y
        // por tanto no lo puede llenar nadie más.
        assert_eq!(
            candidate(b"a.txt", 9, budget(), &id()),
            b"a.txt.1726000000123-7"
        );
        let largo = vec![b'x'; 255];
        for k in [1u32, 2, 9] {
            let c = candidate(&largo, k, budget(), &id());
            assert!(
                c.len() + ".trashinfo".len() <= 255,
                "candidato {k} de {} bytes",
                c.len()
            );
        }
        // Un recorte jamás deja un nombre que el FS no acepta.
        assert_eq!(candidate(b"", 1, budget(), &id()), b"trashed");
    }

    /// MAJOR-1 del encoding-auditor: un nombre UTF-8 largo se recorta por
    /// FRONTERA de carácter. Partirlo produce un nombre que un vfat `utf8=1` o
    /// un ext4 con `casefold` rechazan, y entonces un fichero que el volumen
    /// aceptó se vuelve intrasheable.
    #[test]
    fn un_recorte_no_parte_un_caracter() {
        // 85 × U+3042 = 255 bytes; el presupuesto (245) cae a mitad del 82º.
        let largo = "あ".repeat(85).into_bytes();
        assert_eq!(largo.len(), 255);
        let c = candidate(&largo, 1, budget(), &id());
        assert!(c.len() <= budget());
        std::str::from_utf8(&c).expect("el recorte deja UTF-8 válido");
        assert_eq!(c.len() % 3, 0, "cortó en frontera: {}", c.len());

        // Y un nombre que NUNCA fue UTF-8 se corta por bytes, sin intentar
        // decodificarlo: el volumen ya lo aceptaba así (regla 1).
        let mut crudo = vec![b'a'; 245];
        crudo.extend_from_slice(&[0xff; 10]);
        let c = candidate(&crudo, 1, budget(), &id());
        assert_eq!(c.len(), 245);
        assert_eq!(c, vec![b'a'; 245]);
    }

    #[test]
    fn la_fecha_de_borrado_es_iso_local() {
        let s = local_datetime(1_726_000_000_123);
        assert_eq!(s.len(), 19, "{s}");
        assert_eq!(&s[4..5], "-");
        assert_eq!(&s[10..11], "T");
        // El MISMO id da la MISMA fecha: es lo que hace idempotente el reintento.
        assert_eq!(s, local_datetime(1_726_000_000_123));
        // Hora LOCAL, así que la del epoch depende de la zona del runner: lo
        // que no depende de ella es la FORMA y que un día sea un día.
        assert!(local_datetime(0).starts_with("19"), "{}", local_datetime(0));
        assert_ne!(s, local_datetime(1_726_000_000_123 + 86_400_000));
    }

    #[test]
    fn el_sidecar_solo_se_deduce_de_una_ruta_con_forma_de_papelera() {
        assert_eq!(
            sidecar_of(Path::new("/t/Trash/files/a.txt")),
            Some(Path::new("/t/Trash/info/a.txt.trashinfo").to_path_buf())
        );
        assert_eq!(sidecar_of(Path::new("/t/otro/a.txt")), None);
    }

    /// Un `.Trash` que es un SYMLINK no se usa: cae en `.Trash-$uid`, que es
    /// lo que impide redirigir la papelera de un montaje compartido.
    #[test]
    fn un_trash_compartido_que_es_symlink_no_se_usa() {
        let dir = tempfile::tempdir().expect("tempdir");
        let top = dir.path();
        std::os::unix::fs::symlink("/tmp", top.join(".Trash")).expect("symlink");
        assert_eq!(shared_topdir_trash(top, 1000), None, "cae en .Trash-$uid");
    }

    /// Una víctima en OTRO dispositivo no cruza: usa la papelera del topdir de
    /// su montaje (`.Trash-$uid`), que es lo que evita el copy+delete
    /// entre dispositivos —largo e incancelable a mitad— que ADR 0009
    /// anotaba como excepción de plataforma.
    ///
    /// Depende del entorno: hace falta un segundo dispositivo montado. Con uno
    /// solo, skip limpio (como el resto de los tests de papelera del crate).
    #[test]
    fn una_victima_en_otro_dispositivo_usa_el_topdir_de_su_montaje() {
        use super::{prepare_trash_dir, uid};
        let otro = Path::new("/dev/shm");
        let casa = tempfile::tempdir().expect("tempdir");
        let (Ok(a), Ok(b)) = (std::fs::metadata(otro), std::fs::metadata(casa.path())) else {
            eprintln!("skip: sin segundo dispositivo montado");
            return;
        };
        {
            use std::os::unix::fs::MetadataExt as _;
            if a.dev() == b.dev() {
                eprintln!("skip: /dev/shm y el tempdir están en el mismo dispositivo");
                return;
            }
        }
        let victima = tempfile::tempdir_in(otro).expect("tempdir en /dev/shm");
        let esperada = otro.join(format!(".Trash-{}", uid()));
        let existia = esperada.exists();
        let dir = prepare_trash_dir(&victima.path().join("v.txt"), Some(casa.path()))
            .expect("hay papelera");
        assert_eq!(
            dir, esperada,
            "la papelera es la del montaje de la víctima, no la de casa"
        );
        assert!(dir.join(super::FILES).is_dir(), "y queda lista");
        if !existia {
            let _ = std::fs::remove_dir_all(&esperada);
        }
    }

    /// En el mismo dispositivo manda la papelera de casa.
    #[test]
    fn en_el_mismo_dispositivo_manda_la_papelera_de_casa() {
        use super::prepare_trash_dir;
        let casa = tempfile::tempdir().expect("tempdir");
        let dir =
            prepare_trash_dir(&casa.path().join("v.txt"), Some(casa.path())).expect("hay papelera");
        assert_eq!(dir, casa.path().join("Trash"));
        assert!(dir.join(super::INFO).is_dir());
    }

    /// BLOCKER del security-reviewer: un directorio de papelera de topdir que
    /// no es NUESTRO no se usa. Aquí se prueba la mitad que un test sin
    /// privilegios puede construir —un symlink en su sitio—, que es la que
    /// `create_dir_all` seguía alegremente: sembrar `/tmp/.Trash-1000 ->
    /// /home/atacante/botín` mandaba ahí los ficheros de otro usuario.
    #[test]
    fn una_papelera_de_topdir_que_no_es_nuestra_no_se_usa() {
        use super::ensure_dir_owned;
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let ajeno = dir.path().join("ajeno");
        std::fs::create_dir(&ajeno).expect("mkdir");
        std::os::unix::fs::symlink(&ajeno, dir.path().join(".Trash-1000")).expect("symlink");
        assert_eq!(
            ensure_dir_owned(&dir.path().join(".Trash-1000")),
            Err(norte_proto::Error::Unsupported),
            "un symlink no es una papelera nuestra"
        );

        // Y una que sí es nuestra pero se quedó abierta se APRIETA a 0700 en
        // vez de rechazarse: los nombres de lo que uno borra no son de nadie más.
        let mia = dir.path().join(".Trash-2000");
        std::fs::create_dir(&mia).expect("mkdir");
        std::fs::set_permissions(&mia, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        ensure_dir_owned(&mia).expect("es nuestra");
        let modo = std::fs::symlink_metadata(&mia)
            .expect("stat")
            .permissions()
            .mode();
        assert_eq!(modo & 0o777, 0o700, "apretada: {modo:o}");
    }

    /// Y un `.Trash` sin sticky tampoco (spec): un directorio compartido sin
    /// sticky deja que cualquiera borre lo de los demás.
    #[test]
    fn un_trash_compartido_sin_sticky_no_se_usa() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("tempdir");
        let top = dir.path();
        std::fs::create_dir(top.join(".Trash")).expect("mkdir");
        std::fs::set_permissions(top.join(".Trash"), std::fs::Permissions::from_mode(0o777))
            .expect("chmod");
        assert_eq!(shared_topdir_trash(top, 1000), None, "cae en .Trash-$uid");
        // Con sticky sí: `$top/.Trash/$uid`.
        std::fs::set_permissions(top.join(".Trash"), std::fs::Permissions::from_mode(0o1777))
            .expect("chmod sticky");
        assert_eq!(
            shared_topdir_trash(top, 1000),
            Some(top.join(".Trash").join("1000"))
        );
    }
}
