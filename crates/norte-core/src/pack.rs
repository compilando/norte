//! Fabricar ficheros: empaquetar, comprobar, partir y juntar (#132).
//!
//! Las cuatro operaciones que las teclas de los presets piden y norte no
//! tenía. Ninguna escribe DENTRO de un contenedor —`norte-vfs-archive` sigue
//! siendo `READ_ONLY` (ADR 0018)—: las cuatro leen por un provider y
//! **fabrican ficheros nuevos** por otro, que puede ser cualquiera.
//!
//! Desempaquetar no está aquí porque no hace falta: el motor de copia ya
//! acepta el interior de un archivo como ORIGEN, así que desempaquetar es un
//! `fs.copy` desde `<contenedor>/!/` y hereda el journal, el undo, la política
//! de colisiones y la cancelación que la copia ya tiene.

use std::sync::Arc;

use futures::StreamExt as _;
use norte_proto::{EntryKind, Error, VPath, methods};
use norte_vfs::Provider;
use norte_vfs_archive::write::{ArchiveWriter, PackEntry, PackFormat};

use crate::observer::{Mutation, MutationObserver};
use crate::scheduler::TaskCtx;

/// Tope de bytes del nombre de una entrada, el MISMO con el que el índice de
/// lectura mira un archivo (`norte_vfs_archive::Limits`).
///
/// Escribir por encima produce entradas que este mismo programa omitirá al
/// abrirlo: un archivo que se traga ficheros en silencio.
const MAX_NOMBRE_ENTRADA: usize = 4_096;

/// Tope de componentes de una entrada, por lo mismo.
const MAX_PROFUNDIDAD_ENTRADA: usize = 64;

/// Nivel de compresión por defecto cuando el cliente no dice ninguno.
const NIVEL_POR_DEFECTO: u8 = 6;

/// El formato del wire, traducido al del escritor.
fn formato(f: methods::ArchiveFormat) -> PackFormat {
    match f {
        methods::ArchiveFormat::Zip => PackFormat::Zip,
        methods::ArchiveFormat::Tar => PackFormat::Tar,
        methods::ArchiveFormat::TarGz => PackFormat::TarGz,
    }
}

/// El nombre que una ruta tiene DENTRO del archivo: lo que hay de `base` a
/// `p`, en bytes crudos y separado por `/`.
///
/// `None` si `p` no cuelga de `base` — el llamante lo rechaza en vez de
/// inventarse un nombre, porque un nombre inventado acaba en un archivo que
/// alguien desempaqueta encima de otra cosa.
fn nombre_relativo(base: &VPath, p: &VPath) -> Option<Vec<u8>> {
    if p.scheme() != base.scheme() || p.authority() != base.authority() {
        return None;
    }
    let base_segs: Vec<&[u8]> = base.segments().collect();
    let segs: Vec<&[u8]> = p.segments().collect();
    if segs.len() <= base_segs.len() || !segs.starts_with(&base_segs) {
        return None;
    }
    let cola = &segs[base_segs.len()..];
    // El marcador de archivo NO puede ser el nombre de una entrada: el índice
    // de lectura omite justo ese componente (ADR 0018), así que escribirlo
    // produciría una entrada que norte no puede volver a direccionar.
    if cola.iter().any(|s| *s == b"!") {
        return None;
    }
    let mut out = Vec::new();
    for (i, s) in cola.iter().enumerate() {
        if i > 0 {
            out.push(b'/');
        }
        out.extend_from_slice(s);
    }
    Some(out)
}

/// El token de formato que sugiere el NOMBRE de un contenedor, con los alias
/// que la gente escribe de verdad (`.tgz`, `.tar.gz`).
///
/// Gemelo del que usa la TUI para decidir si `Enter` entra en un fichero
/// (`nav::archive_root_for`), y aquí por la misma razón que allí: es azúcar de
/// presentación sobre la whitelist de proto, no una validación — de eso se
/// encarga `archive_compose`.
pub(crate) fn formato_de_nombre(name: &[u8]) -> Option<&'static str> {
    const ALIAS: &[(&[u8], &str)] = &[(b".tar.gz", "tar+gz"), (b".tgz", "tar+gz")];
    let acaba = |suf: &[u8]| {
        name.len() >= suf.len() && name[name.len() - suf.len()..].eq_ignore_ascii_case(suf)
    };
    ALIAS
        .iter()
        .find(|(suf, _)| acaba(suf))
        .map(|(_, f)| *f)
        .or_else(|| {
            norte_proto::ARCHIVE_FORMATS
                .iter()
                .find(|f| acaba(format!(".{f}").as_bytes()))
                .copied()
        })
}

/// QUÉ se puede comprobar de verdad en cada formato.
///
/// Va al resultado y no a la documentación porque «pasa» significa cosas
/// distintas: un zip trae un CRC-32 por entrada y un `tar.gz` uno de todo el
/// flujo, pero un tar plano no trae ninguna suma de contenido — lo único
/// verificable ahí es que cada tamaño declarado se alcanza. Un cliente que
/// pintara «íntegro» sobre eso estaría afirmando lo que el formato no sostiene.
pub(crate) fn que_se_comprueba(token: &str) -> Vec<String> {
    let v = match token {
        "zip" => "crc",
        "tar+gz" => "gzip_crc",
        // tar plano y rar delegado: solo que los tamaños se alcanzan.
        _ => "sizes",
    };
    vec![v.to_owned()]
}

/// A dónde se escribe: el provider y la ruta.
///
/// Un tipo y no dos parámetros sueltos porque `pack` ya llevaba ocho, y los
/// dos que van juntos son exactamente estos: el provider es el DE esa ruta.
pub(crate) struct Destino {
    /// El provider del destino, que puede no ser el de las fuentes.
    pub(crate) provider: Arc<dyn Provider>,
    /// El fichero que se crea.
    pub(crate) dest: VPath,
}

/// Cómo se empaqueta: contra qué base se nombran las entradas, en qué formato
/// y con cuánta compresión.
pub(crate) struct Empaquetado {
    /// El directorio del que cuelgan los nombres guardados.
    pub(crate) base: VPath,
    /// Formato, decidido por el cliente.
    pub(crate) format: methods::ArchiveFormat,
    /// Nivel 0..=9, o el del core.
    pub(crate) level: Option<u8>,
}

/// Una entrada del recorrido: qué escribir y de dónde leerlo.
struct Pieza {
    provider: Arc<dyn Provider>,
    path: VPath,
    entry: PackEntry,
}

/// Recorre las raíces y enumera TODO lo que va a entrar en el archivo, antes
/// de escribir un solo byte.
///
/// Enumerar primero cuesta un recorrido y compra tres cosas: el total de
/// entradas para la barra (una barra sin total es una barra que no informa),
/// el rechazo de un nombre imposible ANTES de haber creado el destino, y un
/// orden estable.
async fn enumera(
    fuentes: Vec<(Arc<dyn Provider>, VPath)>,
    base: &VPath,
    ctx: &TaskCtx,
) -> Result<Vec<Pieza>, Error> {
    // Lo que este ACTOR no puede recorrer (#209, y antes #165): el gate de
    // lectura mira la RAÍZ de la petición y nada más, así que empaquetar
    // `$HOME` es legítimo y se llevaba por delante el directorio de estado del
    // daemon con él — `journal.db`, `secrets.age`, `connections.toml`,
    // `session.json`. Y un archivo es peor que una comparación: el agente lo
    // vuelve a leer entrada por entrada por el provider de archivos, sobre un
    // fichero que está en su propio scope. Un `fs.read` de cualquiera de esos
    // ficheros se deniega; sin esto, `archive.pack` los blanqueaba todos.
    //
    // Sale del MISMO sitio que las exclusiones de `fs.search` y `fs.compare`:
    // dos listas de lo que un agente no puede recorrer serían dos listas que
    // divergen.
    let excluidas = crate::policy::walk_exclusions(&ctx.actor);
    let mut out = Vec::new();
    for (provider, raiz) in fuentes {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut pendientes = vec![raiz];
        while let Some(p) = pendientes.pop() {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            // Se comprueba ANTES del `stat`: que la entrada exista tampoco es
            // asunto de quien no puede recorrerla.
            if excluidas
                .iter()
                .any(|raiz| crate::policy::is_under(raiz, &p))
            {
                tracing::debug!("archive.pack: subárbol excluido para este actor");
                continue;
            }
            let e = provider.stat(&p).await?;
            let nombre = nombre_relativo(base, &p).ok_or(Error::InvalidPath)?;
            // Los MISMOS topes que el índice de lectura (ADR 0018): un nombre
            // más largo que `max_name_bytes` o una profundidad por encima de
            // `max_depth` se OMITEN al leer, así que escribirlos produce un
            // archivo cuyas entradas norte no vuelve a ver — el mismo agujero
            // que el rechazo del marcador `!` cierra, por otra puerta.
            // `split` y no un contador de bytes: son nombres de ruta, no un
            // flujo, y la sugerencia de clippy (traerse `bytecount`) es una
            // dependencia entera para contar barras en 4 KiB.
            let hondura = nombre.split(|b| *b == b'/').count();
            if nombre.len() > MAX_NOMBRE_ENTRADA || hondura > MAX_PROFUNDIDAD_ENTRADA {
                tracing::warn!("archive.pack: una entrada no cabría en el índice de lectura");
                return Err(Error::InvalidPath);
            }
            match e.kind {
                EntryKind::Dir => {
                    out.push(Pieza {
                        provider: Arc::clone(&provider),
                        path: p.clone(),
                        entry: PackEntry::dir(nombre),
                    });
                    let mut stream = provider.list(&p).await?;
                    while let Some(hijo) = stream.next().await {
                        pendientes.push(hijo?.path);
                    }
                }
                EntryKind::File => {
                    let mut pe = PackEntry::file(nombre, e.size.unwrap_or(0));
                    pe.mtime_ms = e.mtime_ms;
                    out.push(Pieza {
                        provider: Arc::clone(&provider),
                        path: p,
                        entry: pe,
                    });
                }
                // Un symlink no se sigue ni se guarda como enlace: guardar el
                // target sería copiar lo apuntado sin decirlo, y guardar el
                // enlace pide un tipo de entrada que el escritor no tiene
                // todavía. Se OMITE con aviso, que es lo que hace el índice de
                // lectura con lo que no sabe representar.
                EntryKind::Symlink | EntryKind::Other => {
                    tracing::warn!("archive.pack: entrada omitida por su tipo");
                }
            }
        }
    }
    // Los directorios primero dentro de cada nivel, y estable: un archivo cuyo
    // orden depende del orden de listado del provider no es reproducible.
    out.sort_by(|a, b| a.entry.name.cmp(&b.entry.name));
    // **Dos entradas con el MISMO nombre guardado no se escriben.** Pasa con
    // raíces que se solapan —`sources: ["/p/a", "/p/a/b"]`, que el wire acepta
    // aunque las marcas de la TUI no lo formen— y el archivo resultante lleva
    // la entrada dos veces, con su contenido dos veces: nuestro índice resuelve
    // «gana la última» y otras herramientas la extraen dos veces. Ordenado
    // como está, encontrarlo es una comparación.
    if out.windows(2).any(|p| p[0].entry.name == p[1].entry.name) {
        tracing::warn!("archive.pack: dos fuentes dan el mismo nombre dentro del archivo");
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    // **Y dos que PLIEGAN al mismo nombre tampoco** (#250). El caso de arriba
    // es que los bytes coincidan; éste es que coincidan allí donde el archivo
    // se vaya a extraer, que es lo que un archivo no puede saber: `café.txt`
    // en NFD y en NFC son dos ficheros en ext4 y uno en APFS, `µ` y `μ` son
    // dos aquí y uno en NTFS, y `straße` y `strasse` son dos en casi todas
    // partes y una en un ext4 `+F`. Extraído allí, uno de los dos desaparece
    // sin decir nada.
    //
    // Se pliega con el modo MÁS ANCHO a propósito: el destino de un archivo es
    // por definición desconocido —se manda por ahí—, así que la pregunta no es
    // «¿colisionan en esta máquina?» sino «¿colisionan en alguna?». El precio
    // es rechazar una pareja que aquí es legítima; el de no hacerlo es un
    // fichero perdido en silencio en la máquina de otro, y ésa es la dirección
    // que ADR 0005 dice no tomar.
    let mut claves: Vec<Vec<u8>> = out
        .iter()
        .map(|p| {
            norte_encoding::name_key(&p.entry.name, norte_encoding::FoldMode::Full).into_owned()
        })
        .collect();
    claves.sort_unstable();
    if claves.windows(2).any(|k| k[0] == k[1]) {
        tracing::warn!("archive.pack: dos entradas serían el mismo nombre al extraerlas");
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    Ok(out)
}

/// `archive.pack`: fabrica el archivo.
///
/// El destino se escribe por un [`norte_vfs::ByteSink`], así que la
/// cancelación deja el destino LIMPIO —`abort` se lleva el staging— y no un
/// fichero a medias que parezca un archivo. La entrada del journal se emite
/// después del `commit`, que es cuando el nodo existe de verdad.
pub(crate) async fn pack(
    fuentes: Vec<(Arc<dyn Provider>, VPath)>,
    destino: Destino,
    que: Empaquetado,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let Destino {
        provider: provider_destino,
        dest,
    } = destino;
    let Empaquetado {
        base,
        format,
        level,
    } = que;
    if fuentes.is_empty() {
        return Err(Error::InvalidPath);
    }
    // El veredicto del journal, fijado antes del primer efecto (#205).
    let observer = crate::observer::pin_for_task(observer).await?;
    // El destino NO se sobrescribe: fabricar un archivo encima de un fichero
    // que ya está es pérdida silenciosa, y quien llama ya sabe preguntar.
    if provider_destino.stat(&dest).await.is_ok() {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    let piezas = enumera(fuentes, &base, ctx).await?;
    let total_bytes: u64 = piezas.iter().map(|p| p.entry.size).sum();
    ctx.progress.update(|p| {
        p.entries_total = Some(piezas.len() as u64);
        p.bytes_total = Some(total_bytes);
    });

    let mut w = ArchiveWriter::new(
        formato(format),
        u32::from(level.unwrap_or(NIVEL_POR_DEFECTO)),
    );
    let mut sink = provider_destino.write(&dest).await?;
    let mut leidos: u64 = 0;
    let mut hechas: u64 = 0;

    // Un cierre para no repetir el «suelta el sink sin publicar» de cada
    // camino de salida: sin esto, un `?` en medio dejaría el staging colgando.
    macro_rules! abortando {
        ($e:expr) => {{
            let err = $e;
            let _ = sink.abort().await;
            return Err(err);
        }};
    }

    for pieza in piezas {
        if let Err(e) = una_pieza(&pieza, &mut w, &mut *sink, &mut leidos, ctx).await {
            abortando!(e);
        }
        hechas = hechas.saturating_add(1);
        ctx.progress.update(|p| p.entries_done = hechas);
    }
    if let Err(e) = w.finish() {
        abortando!(de_pack(e));
    }
    let salida = w.take();
    if !salida.is_empty()
        && let Err(e) = sink.write(bytes::Bytes::from(salida)).await
    {
        abortando!(e);
    }
    sink.commit().await?;
    // Después del commit: antes, el journal apuntaría a un nodo que todavía no
    // existe (regla 4).
    observer
        .on_mutation(&Mutation::Created(&dest), &ctx.actor)
        .await?;
    Ok(())
}

/// UNA entrada: se abre, se le pasan los bytes del origen a trozos, y lo que
/// el escritor va produciendo se drena al sink según sale.
///
/// Aparte del bucle de [`pack`] para que ninguna de las dos pase de cien
/// líneas, y porque es la unidad que se lee entera de un vistazo: abrir,
/// copiar, cerrar. El dueño del sink es quien llama — un error aquí ABORTA el
/// archivo, no se salta la entrada.
async fn una_pieza(
    pieza: &Pieza,
    w: &mut ArchiveWriter,
    sink: &mut dyn norte_vfs::ByteSink,
    leidos: &mut u64,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    if ctx.cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    ctx.progress
        .update(|p| p.current = Some(pieza.path.clone()));
    w.begin(&pieza.entry).map_err(de_pack)?;
    if !pieza.entry.dir {
        let mut stream = pieza.provider.read(&pieza.path, None).await?;
        let mut escritos: u64 = 0;
        while let Some(chunk) = stream.next().await {
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let chunk = chunk?;
            escritos = escritos.saturating_add(chunk.len() as u64);
            w.data(&chunk).map_err(de_pack)?;
            *leidos = leidos.saturating_add(chunk.len() as u64);
            let hechos = *leidos;
            ctx.progress.update(|p| p.bytes_done = hechos);
            let salida = w.take();
            if !salida.is_empty() {
                sink.write(bytes::Bytes::from(salida)).await?;
            }
        }
        // El origen cambió entre el `stat` y la lectura. En tar eso es una
        // cabecera que miente sobre lo que viene detrás, así que el archivo
        // entero deja de poderse leer más allá de esa entrada: se aborta en vez
        // de publicar algo así.
        if escritos != pieza.entry.size {
            tracing::warn!("archive.pack: el origen cambió de tamaño mientras se leía");
            return Err(Error::Conflict {
                conflict: norte_proto::ConflictKind::TypeMismatch,
            });
        }
    }
    w.end().map_err(de_pack)?;
    let salida = w.take();
    if !salida.is_empty() {
        sink.write(bytes::Bytes::from(salida)).await?;
    }
    Ok(())
}

/// Un fallo del escritor, en la taxonomía del wire.
fn de_pack(e: norte_vfs_archive::write::PackError) -> Error {
    use norte_vfs_archive::write::PackError as P;
    match e {
        // Un nombre que no cabe en el formato es una petición imposible, no un
        // fallo de I/O.
        P::Nombre => Error::InvalidPath,
        // NO retryable: reintentar produce exactamente el mismo fallo. Un
        // `Estado` es un bug de este código y un `Tamano` es un origen que se
        // movió bajo los pies.
        P::Tamano | P::Estado | P::Io => Error::Io { retryable: false },
    }
}

/// `archive.test`: lee cada entrada hasta el final y dice qué se comprobó.
///
/// La comprobación de verdad la hace el LECTOR: el de zip verifica el CRC-32
/// cuando una entrada se lee entera, y el de `tar.gz` la cola del gzip. Este
/// op recorre y recoge; duplicar aquí la verificación sería tener dos
/// opiniones sobre lo mismo.
pub(crate) async fn test_archive(
    provider: Arc<dyn Provider>,
    raiz: VPath,
    checked: Vec<String>,
    informe: Arc<std::sync::Mutex<methods::ArchiveTestResult>>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    {
        let mut i = informe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        i.checked = checked;
    }
    let mut pendientes = vec![raiz];
    let mut entradas: u64 = 0;
    let mut bytes: u64 = 0;
    while let Some(dir) = pendientes.pop() {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let mut stream = provider.list(&dir).await?;
        while let Some(e) = stream.next().await {
            let e = e?;
            if ctx.cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            match e.kind {
                EntryKind::Dir => pendientes.push(e.path),
                EntryKind::File => {
                    entradas = entradas.saturating_add(1);
                    ctx.progress.update(|p| {
                        p.entries_done = entradas;
                        p.current = Some(e.path.clone());
                    });
                    if let Err(err) = lee_entera(&*provider, &e.path, &mut bytes, ctx).await {
                        if matches!(err, Error::Cancelled) {
                            return Err(Error::Cancelled);
                        }
                        anota(&informe, &e.path, &err);
                    }
                    ctx.progress.update(|p| p.bytes_done = bytes);
                }
                EntryKind::Symlink | EntryKind::Other => {
                    entradas = entradas.saturating_add(1);
                }
            }
        }
    }
    let mut i = informe
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    i.entries = entradas;
    Ok(())
}

/// Lee una entrada entera, que es lo que dispara la verificación del lector.
async fn lee_entera(
    provider: &dyn Provider,
    path: &VPath,
    bytes: &mut u64,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let mut stream = provider.read(path, None).await?;
    while let Some(chunk) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        *bytes = bytes.saturating_add(chunk?.len() as u64);
    }
    Ok(())
}

/// Apunta un fallo en el informe, con el tope puesto.
fn anota(informe: &Arc<std::sync::Mutex<methods::ArchiveTestResult>>, path: &VPath, err: &Error) {
    let razon = match err {
        Error::Corrupt => "crc",
        Error::Unsupported => "unsupported",
        Error::NotFound => "truncated",
        _ => "io",
    };
    let mut i = informe
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if i.failed.len() >= methods::ARCHIVE_TEST_MAX_FAILURES {
        i.truncated = true;
        return;
    }
    i.failed.push(methods::ArchiveTestFailure {
        // La ruta ENTERA y en forma wire: es lo único que conserva los bytes,
        // y este informe es el único sitio donde se nombra la entrada que
        // falló. El `name` con pérdidas va aparte, para enseñarlo.
        path: path.to_wire(),
        name: path
            .file_name()
            .map(|s| String::from_utf8_lossy(s.as_bytes()).into_owned())
            .unwrap_or_default(),
        reason: razon.to_owned(),
    });
}

/// El nombre del trozo `n` de un split: `<nombre>.001`.
fn nombre_trozo(base: &[u8], n: u64) -> Vec<u8> {
    let mut v = base.to_vec();
    v.extend_from_slice(format!(".{n:03}").as_bytes());
    v
}

/// `file.split`: parte un fichero en trozos numerados.
pub(crate) async fn split(
    src: Arc<dyn Provider>,
    path: VPath,
    part_bytes: u64,
    provider_destino: Arc<dyn Provider>,
    dest_dir: VPath,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let observer = crate::observer::pin_for_task(observer).await?;
    let (nombre, total, trozos) = mide_el_reparto(&*src, &path, part_bytes).await?;
    ctx.progress.update(|p| {
        p.bytes_total = Some(total);
        p.entries_total = Some(trozos);
    });

    sitio_libre(&*provider_destino, &dest_dir, &nombre, trozos).await?;

    // **Sin acumular.** La primera versión juntaba `part_bytes` en un `Vec` y
    // luego lo drenaba: pico de dos veces el tamaño del trozo, y con un
    // `part_bytes` que el wire no acota —`u64::MAX` es un valor legal— un
    // cliente cualquiera se llevaba el daemon por delante con un OOM. Ahora se
    // escribe según llega y el trozo se cierra cuando se llena, así que la
    // memoria es la de UN chunk del provider. De paso, la cancelación se mira
    // por chunk y no por trozo: con trozos de un giga, esperar al final del
    // trozo es no cancelar.
    let mut stream = src.read(&path, None).await?;
    let mut hechos: u64 = 0;
    let mut escritos: u64 = 0;
    let mut sink: Option<Box<dyn norte_vfs::ByteSink>> = None;
    let mut en_curso: u64 = 0;
    let mut destino_actual: Option<VPath> = None;
    // Los que ya están publicados, para poder retirarlos si esto se corta:
    // medio conjunto de trozos es indistinguible de uno entero (ver
    // [`retira_los_trozos`]).
    let mut publicados: Vec<VPath> = Vec::new();

    macro_rules! deshaciendo {
        ($sink:expr, $publicados:expr, $e:expr) => {{
            if let Some(s) = $sink.take() {
                let _ = s.abort().await;
            }
            retira_los_trozos(&*provider_destino, &$publicados, &observer, ctx).await;
            return Err($e);
        }};
    }

    while let Some(chunk) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            deshaciendo!(sink, publicados, Error::Cancelled);
        }
        let chunk = match chunk {
            Ok(c) => c,
            Err(e) => deshaciendo!(sink, publicados, e),
        };
        let mut resto = &chunk[..];
        while !resto.is_empty() {
            if sink.is_none() {
                if hechos >= methods::FILE_SPLIT_MAX_PARTS {
                    // El tope, contra lo que se está ESCRIBIENDO y no contra
                    // la estimación del `stat`: un fichero que crece mientras
                    // se lee pasaba la estimación con 800 trozos y escribía
                    // 1200, y `.1000` no lo vuelve a juntar nadie.
                    deshaciendo!(
                        sink,
                        publicados,
                        Error::LimitExceeded {
                            limit: "split-parts".to_owned(),
                        }
                    );
                }
                let destino = dest_dir.join(
                    norte_proto::Segment::new(nombre_trozo(&nombre, hechos + 1))
                        .map_err(|_| Error::InvalidPath)?,
                );
                sink = Some(provider_destino.write(&destino).await?);
                destino_actual = Some(destino);
                en_curso = 0;
            }
            let cabe = usize::try_from(part_bytes - en_curso).unwrap_or(usize::MAX);
            let corte = cabe.min(resto.len());
            let (ahora, luego) = resto.split_at(corte);
            let fallo = match sink.as_mut() {
                Some(s) => s.write(bytes::Bytes::copy_from_slice(ahora)).await.err(),
                None => None,
            };
            if let Some(e) = fallo {
                deshaciendo!(sink, publicados, e);
            }
            en_curso += ahora.len() as u64;
            escritos = escritos.saturating_add(ahora.len() as u64);
            resto = luego;
            if en_curso == part_bytes {
                let cerrado = cierra_trozo(
                    &mut sink,
                    destino_actual.take(),
                    &observer,
                    &mut hechos,
                    escritos,
                    ctx,
                )
                .await;
                match cerrado {
                    Ok(Some(p)) => publicados.push(p),
                    Ok(None) => {}
                    Err(e) => deshaciendo!(sink, publicados, e),
                }
            }
        }
        ctx.progress.update(|p| p.bytes_done = escritos);
    }
    // El último, que casi nunca está lleno. Si la división fue exacta no queda
    // ninguno abierto, y por eso NO se escribe un trozo vacío al final.
    let cerrado = cierra_trozo(
        &mut sink,
        destino_actual.take(),
        &observer,
        &mut hechos,
        escritos,
        ctx,
    )
    .await;
    match cerrado {
        Ok(_) => Ok(()),
        Err(e) => deshaciendo!(sink, publicados, e),
    }
}

/// Retira los trozos ya publicados de un split que se cortó.
///
/// Cancelar o fallar a mitad deja un conjunto que PARECE completo, y ésa es la
/// trampa que este op existe para no tender: los trozos escritos son todos del
/// tamaño pedido, no hay hueco, y juntar los tres primeros de diez da un
/// fichero corto que pasa todos los guardas. Un árbol copiado a medias se ve a
/// simple vista; medio conjunto de trozos, no.
///
/// Cada retirada se anota: el journal cuenta lo que hay, no lo que hubo. Lo
/// que no se pueda retirar se dice en el log y no se reintenta — este camino
/// ya está saliendo por un error.
async fn retira_los_trozos(
    provider_destino: &dyn Provider,
    publicados: &[VPath],
    observer: &Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) {
    for p in publicados.iter().rev() {
        match provider_destino.remove(p).await {
            Ok(()) => {
                let _ = observer
                    .on_mutation(&Mutation::Removed(p), &ctx.actor)
                    .await;
            }
            Err(e) => {
                tracing::warn!(error = %e, "file.split: un trozo a medias no se pudo retirar");
            }
        }
    }
}

/// El nombre base, el tamaño y CUÁNTOS trozos van a salir — o por qué no.
///
/// Todo lo que se puede saber antes de escribir un byte, junto: el trozo no es
/// ridículo, el origen es un fichero, y el conjunto cabe en la convención de
/// tres dígitos. Descubrir lo último en el trozo 1000 dejaría un conjunto que
/// nadie puede volver a juntar.
pub(crate) async fn mide_el_reparto(
    src: &dyn Provider,
    path: &VPath,
    part_bytes: u64,
) -> Result<(Vec<u8>, u64, u64), Error> {
    if part_bytes < methods::FILE_SPLIT_MIN_BYTES {
        return Err(Error::InvalidPath);
    }
    let e = src.stat(path).await?;
    if e.kind != EntryKind::File {
        return Err(Error::InvalidPath);
    }
    let total = e.size.unwrap_or(0);
    let trozos = total.div_ceil(part_bytes).max(1);
    if trozos > methods::FILE_SPLIT_MAX_PARTS {
        return Err(Error::LimitExceeded {
            limit: "split-parts".to_owned(),
        });
    }
    let nombre = path
        .file_name()
        .map(|s| s.as_bytes().to_vec())
        .ok_or(Error::InvalidPath)?;
    Ok((nombre, total, trozos))
}

/// Ningún trozo del conjunto puede existir ya.
///
/// Se comprueba ANTES de escribir el primero: descubrirlo en el cuarto deja
/// tres trozos nuevos mezclados con los rancios de una tanda anterior, y ese
/// conjunto se junta sin que nada chirríe.
async fn sitio_libre(
    provider_destino: &dyn Provider,
    dest_dir: &VPath,
    nombre: &[u8],
    trozos: u64,
) -> Result<(), Error> {
    for i in 1..=trozos {
        let p = dest_dir.join(
            norte_proto::Segment::new(nombre_trozo(nombre, i)).map_err(|_| Error::InvalidPath)?,
        );
        if provider_destino.stat(&p).await.is_ok() {
            return Err(Error::Conflict {
                conflict: norte_proto::ConflictKind::Exists,
            });
        }
    }
    Ok(())
}

/// Publica el trozo abierto —si lo hay— y lo anota en el journal.
///
/// El `Created` va DESPUÉS del commit, que es cuando el nodo existe (regla 4).
async fn cierra_trozo(
    sink: &mut Option<Box<dyn norte_vfs::ByteSink>>,
    destino: Option<VPath>,
    observer: &Arc<dyn MutationObserver>,
    hechos: &mut u64,
    escritos: u64,
    ctx: &TaskCtx,
) -> Result<Option<VPath>, Error> {
    let (Some(s), Some(destino)) = (sink.take(), destino) else {
        return Ok(None);
    };
    s.commit().await?;
    observer
        .on_mutation(&Mutation::Created(&destino), &ctx.actor)
        .await?;
    *hechos += 1;
    let n = *hechos;
    ctx.progress.update(|p| {
        p.entries_done = n;
        p.bytes_done = escritos;
        p.current = Some(destino.clone());
    });
    Ok(Some(destino))
}

/// ¿Hay algún trozo numerado POR ENCIMA de `hasta`?
///
/// Es la comprobación del hueco, y se hace listando: derivar los nombres de
/// uno en uno hasta 999 serían 999 `stat` contra un provider remoto, y parar
/// antes es justo el bug. El nombre se compara en BYTES contra
/// `<base>.NNN` — nada se decodifica (regla 1).
async fn hay_trozos_por_encima(
    src: &dyn Provider,
    dir: &VPath,
    base: &[u8],
    hasta: u64,
    ctx: &TaskCtx,
) -> Result<bool, Error> {
    let mut stream = src.list(dir).await?;
    while let Some(e) = stream.next().await {
        if ctx.cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let e = e?;
        let Some(nombre) = e.path.file_name().map(|s| s.as_bytes().to_vec()) else {
            continue;
        };
        // `<base>.NNN` y nada más: `x.iso.001` cuenta, `x.iso.001.bak` no.
        let Some(cola) = nombre
            .strip_prefix(base)
            .and_then(|c| c.strip_prefix(b"."))
            .filter(|c| c.len() == 3 && c.iter().all(u8::is_ascii_digit))
        else {
            continue;
        };
        let n: u64 = std::str::from_utf8(cola)
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        if n > hasta {
            tracing::warn!("file.combine: falta un trozo intermedio");
            return Ok(true);
        }
    }
    Ok(false)
}

/// `file.combine`: junta los trozos de un split.
///
/// Los trozos se enumeran y se MIDEN antes de crear el destino: así un hueco
/// —o un trozo intermedio más corto que el primero, que es un trozo perdido—
/// se rechaza sin haber escrito nada. Un fichero mal unido es un fichero
/// corrupto con buena pinta, y eso es peor que un error.
pub(crate) async fn combine(
    src: Arc<dyn Provider>,
    first: VPath,
    provider_destino: Arc<dyn Provider>,
    dest: VPath,
    observer: Arc<dyn MutationObserver>,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let observer = crate::observer::pin_for_task(observer).await?;
    let nombre = first
        .file_name()
        .map(|s| s.as_bytes().to_vec())
        .ok_or(Error::InvalidPath)?;
    // `x.iso.001` → base `x.iso`.
    let base = nombre
        .len()
        .checked_sub(4)
        .filter(|n| nombre[*n] == b'.' && nombre[n + 1..].iter().all(u8::is_ascii_digit))
        .map(|n| nombre[..n].to_vec())
        .ok_or(Error::InvalidPath)?;
    let dir = first.parent().ok_or(Error::InvalidPath)?;
    let trozos = enumera_trozos(&*src, &dir, &base, ctx).await?;
    let total: u64 = trozos.iter().map(|(_, s)| *s).sum();
    ctx.progress.update(|p| {
        p.bytes_total = Some(total);
        p.entries_total = Some(trozos.len() as u64);
    });
    if provider_destino.stat(&dest).await.is_ok() {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::Exists,
        });
    }
    escribe_juntos(&*src, trozos, &*provider_destino, &dest, ctx).await?;
    observer
        .on_mutation(&Mutation::Created(&dest), &ctx.actor)
        .await?;
    Ok(())
}

/// Los trozos del conjunto, en orden y con su tamaño — y todas las razones por
/// las que un conjunto NO se une.
///
/// Aparte de [`combine`] para que ninguna de las dos pase de cien líneas, y
/// porque lo que hay aquí es una sola pregunta con tres formas de contestar
/// que no: falta un trozo, sobra un trozo, o uno de en medio está a medias.
async fn enumera_trozos(
    src: &dyn Provider,
    dir: &VPath,
    base: &[u8],
    ctx: &TaskCtx,
) -> Result<Vec<(VPath, u64)>, Error> {
    let mut trozos: Vec<(VPath, u64)> = Vec::new();
    let mut n = 1_u64;
    loop {
        let p = dir.join(
            norte_proto::Segment::new(nombre_trozo(base, n)).map_err(|_| Error::InvalidPath)?,
        );
        match src.stat(&p).await {
            Ok(e) if e.kind == EntryKind::File => {
                trozos.push((p, e.size.unwrap_or(0)));
                n += 1;
            }
            _ => break,
        }
        // Pasado el tope se REHÚSA, no se corta. Cortar aquí unía los 999
        // primeros de un conjunto de 1200 —de 7-Zip, por ejemplo, que numera
        // hasta `.1000`— y publicaba un fichero corto que pasa todos los
        // guardas: no hay hueco y todos los trozos recogidos miden lo mismo.
        if n > methods::FILE_SPLIT_MAX_PARTS {
            let siguiente = dir.join(
                norte_proto::Segment::new(nombre_trozo(base, n)).map_err(|_| Error::InvalidPath)?,
            );
            if src.stat(&siguiente).await.is_ok() {
                return Err(Error::LimitExceeded {
                    limit: "split-parts".to_owned(),
                });
            }
            break;
        }
    }
    if trozos.is_empty() {
        return Err(Error::NotFound);
    }
    // **Un HUECO no se une a través, y encontrarlo pide MIRAR.** El paseo de
    // arriba se para en el primer número que falta, así que un conjunto
    // `.001 .003 .004` se veía como uno de un solo trozo y se unía: la task
    // decía `Completed`, el journal anotaba un `Created`, y en disco quedaba
    // el 20 % de una ISO que monta como imagen corrupta.
    if hay_trozos_por_encima(src, dir, base, trozos.len() as u64, ctx).await? {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::TypeMismatch,
        });
    }
    // Todos menos el último miden lo mismo que el primero. Un intermedio más
    // corto es un trozo que se copió a medias, y unir a través de él da un
    // fichero que parece entero.
    let primero = trozos[0].1;
    if trozos[..trozos.len() - 1]
        .iter()
        .any(|(_, s)| *s != primero)
    {
        return Err(Error::Conflict {
            conflict: norte_proto::ConflictKind::TypeMismatch,
        });
    }
    Ok(trozos)
}

/// Escribe el destino a partir de los trozos, en orden.
async fn escribe_juntos(
    src: &dyn Provider,
    trozos: Vec<(VPath, u64)>,
    provider_destino: &dyn Provider,
    dest: &VPath,
    ctx: &TaskCtx,
) -> Result<(), Error> {
    let mut sink = provider_destino.write(dest).await?;
    let mut escritos: u64 = 0;
    for (i, (p, _)) in trozos.iter().enumerate() {
        if ctx.cancel.is_cancelled() {
            let _ = sink.abort().await;
            return Err(Error::Cancelled);
        }
        ctx.progress.update(|q| q.current = Some(p.clone()));
        let mut stream = match src.read(p, None).await {
            Ok(s) => s,
            Err(e) => {
                let _ = sink.abort().await;
                return Err(e);
            }
        };
        while let Some(chunk) = stream.next().await {
            // Por CHUNK: un trozo de 700 MB no puede ser un punto en el que
            // cancelar no hace nada durante medio minuto.
            if ctx.cancel.is_cancelled() {
                let _ = sink.abort().await;
                return Err(Error::Cancelled);
            }
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => {
                    let _ = sink.abort().await;
                    return Err(e);
                }
            };
            escritos = escritos.saturating_add(chunk.len() as u64);
            if let Err(e) = sink.write(chunk).await {
                let _ = sink.abort().await;
                return Err(e);
            }
            ctx.progress.update(|q| q.bytes_done = escritos);
        }
        ctx.progress.update(|q| q.entries_done = i as u64 + 1);
    }
    sink.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vp(w: &str) -> VPath {
        VPath::parse(w).expect("wire de test")
    }

    /// El nombre guardado sale de la BASE, y una ruta que no cuelga de ella no
    /// tiene nombre: inventarle uno es meter en el archivo algo que se
    /// desempaqueta donde nadie espera.
    #[test]
    fn el_nombre_guardado_es_relativo_a_la_base() {
        let base = vp("file:///proj");
        assert_eq!(
            nombre_relativo(&base, &vp("file:///proj/src/main.rs")),
            Some(b"src/main.rs".to_vec())
        );
        assert_eq!(
            nombre_relativo(&base, &vp("file:///proj/LEEME")),
            Some(b"LEEME".to_vec())
        );
        assert_eq!(nombre_relativo(&base, &vp("file:///otro/x")), None);
        assert_eq!(nombre_relativo(&base, &base), None, "la base no es entrada");
        assert_eq!(
            nombre_relativo(&base, &vp("mem:///proj/x")),
            None,
            "ni de otro provider"
        );
    }

    /// El marcador `!` no puede ser el nombre de una entrada: el índice de
    /// lectura omite ese componente, así que escribirlo produce algo que norte
    /// no puede volver a nombrar.
    #[test]
    fn el_marcador_no_puede_ser_una_entrada() {
        let base = vp("file:///proj");
        let con_marcador = base.join(norte_proto::Segment::new(b"!".to_vec()).expect("seg"));
        assert_eq!(nombre_relativo(&base, &con_marcador), None);
    }

    /// Lo que se comprueba depende del formato, y el informe lo dice: decir
    /// «pasa» sobre un tar plano sería afirmar una integridad que el formato
    /// no tiene con qué sostener.
    #[test]
    fn cada_formato_dice_que_comprueba() {
        assert_eq!(que_se_comprueba("zip"), vec!["crc".to_owned()]);
        assert_eq!(que_se_comprueba("tar+gz"), vec!["gzip_crc".to_owned()]);
        assert_eq!(que_se_comprueba("tar"), vec!["sizes".to_owned()]);
        assert_eq!(que_se_comprueba("rar"), vec!["sizes".to_owned()]);
    }

    /// El formato del contenedor sale de su nombre, con los alias que la gente
    /// escribe: `.tgz` es `tar+gz`, y la caja da igual.
    #[test]
    fn el_formato_del_contenedor_sale_del_nombre() {
        assert_eq!(formato_de_nombre(b"a.zip"), Some("zip"));
        assert_eq!(formato_de_nombre(b"a.TGZ"), Some("tar+gz"));
        assert_eq!(formato_de_nombre(b"a.tar.gz"), Some("tar+gz"));
        assert_eq!(formato_de_nombre(b"a.tar"), Some("tar"));
        assert_eq!(
            formato_de_nombre(b"a.rar"),
            Some("rar"),
            "leerlo sí se sabe"
        );
        assert_eq!(formato_de_nombre(b"leeme"), None);
    }

    /// Los trozos se numeran a tres dígitos desde el 001, que es la convención
    /// que tienen los usuarios de estas teclas.
    #[test]
    fn los_trozos_se_numeran_como_manda_la_convencion() {
        assert_eq!(nombre_trozo(b"g.iso", 1), b"g.iso.001".to_vec());
        assert_eq!(nombre_trozo(b"g.iso", 42), b"g.iso.042".to_vec());
        assert_eq!(nombre_trozo(b"g.iso", 999), b"g.iso.999".to_vec());
    }
}
