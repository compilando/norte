//! Inicialización de tracing para los binarios.
//!
//! Vive AQUÍ, y no en el core, porque hay dos familias de binarios que lo
//! necesitan y solo una puede depender del motor: la ventana gráfica habla
//! con el daemon por un socket y no puede arrastrar el engine, los providers
//! y el host de plugins para escribir una línea de log (ADR 0066). Montarlo
//! por duplicado fue la decisión anterior, y lo que costó una vez fue que la
//! copia se dejó el endurecimiento: un log legible por cualquier cuenta local
//! con las rutas por las que el usuario había navegado, durante seis commits
//! (#255). Este crate ya era dueño de `state_dir()` y de las claves `[log]`,
//! así que aquí el permiso se pone en UN sitio.
//!
//! Cap de SEGURIDAD (issue #43, regla 10): `suppaftp` loguea cada comando del
//! canal de control a nivel TRACE del crate `log`, incluido `PASS <password>`.
//! El bridge `tracing-log` (feature default de `tracing-subscriber`) lo
//! materializaría con `RUST_LOG=trace`. [`init`](crate::logging::init) añade una directiva estática
//! `suppaftp=info` AL FINAL del filtro, así que gana a cualquier `RUST_LOG`
//! —incluido `suppaftp=trace` explícito— y la password nunca llega al sink.

use std::path::Path;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::prelude::*;

/// Construye el `EnvFilter`: default INFO, respeta `RUST_LOG` (o `env` si se
/// pasa, para tests), y SIEMPRE capa `suppaftp` a `info` como última directiva
/// (regla 10, no configurable).
fn filter_from(env: Option<&str>) -> EnvFilter {
    let base = match env {
        Some(s) => EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .parse_lossy(s),
        None => EnvFilter::builder()
            .with_default_directive(LevelFilter::INFO.into())
            .from_env_lossy(),
    };
    // Última directiva de igual especificidad gana → cap duro.
    base.add_directive("suppaftp=info".parse().expect("directiva estática válida"))
}

/// Prefijo por defecto de los ficheros rotados. La rotación es DIARIA, así
/// que el nombre real lleva la fecha detrás.
const LOG_PREFIX: &str = "norte.log";

/// Crea el directorio del log CERRADO, y aprieta lo que ya haya dentro.
///
/// **0700, y no la umask.** Lo que este log guarda es lo mismo que guarda el
/// journal —cada copia, cada borrado, cada host remoto— y en este árbol todo lo
/// que es estado va cerrado: `journal.db` 0600 en un dir 0700, el spool igual,
/// `lua-trust.toml` igual, `secrets.age` 0600, el socket del daemon 0600. Con
/// la umask de serie esto salía 0755/0644, o sea legible por cualquier cuenta
/// local.
///
/// **Y el modo se aplica a los padres que cree de paso, que es la mitad
/// importante.** `init_to_file` es lo PRIMERO que toca `<state_dir>` en los dos
/// frontends —antes del journal, antes de todo—, así que un `create_dir_all`
/// sin modo creaba `<state_dir>` a 0755; el journal llega después con su
/// `DirBuilder::mode(0o700)`, que sobre un directorio que YA existe no hace
/// chmod ninguno. El 0755 se quedaba para siempre, enseñando el listado de
/// `journal.db`, `lua-trust.toml` y el spool. En una instalación nueva, y sin
/// que nada avisara.
#[cfg(unix)]
fn create_dir_locked(dir: &Path, prefix: &str) -> std::io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt as _, PermissionsExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)?;
    // Un directorio preexistente NO lo toca el `create` de arriba (ése es el
    // agujero que esto cierra), y el appender abre sus ficheros con la umask
    // porque `tracing-appender` no deja elegir modo. Se aprieta lo que haya.
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
    for entrada in std::fs::read_dir(dir)?.flatten() {
        if entrada
            .file_name()
            .as_encoded_bytes()
            .starts_with(prefix.as_bytes())
        {
            let _ =
                std::fs::set_permissions(entrada.path(), std::fs::Permissions::from_mode(0o600));
        }
    }
    Ok(())
}

/// En Windows los permisos son ACLs y `<state_dir>` cuelga de `%LOCALAPPDATA%`,
/// que ya es del usuario. Sin equivalente que aplicar aquí.
#[cfg(not(unix))]
fn create_dir_locked(dir: &Path, _prefix: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)
}

/// La capa de fichero, o `None` si el directorio no se pudo preparar.
///
/// **Escribe SÍNCRONO, sin writer no bloqueante y por tanto sin guard**, y esa
/// es una decisión y no un descuido. Un `WorkerGuard` vacía la cola al soltarlo,
/// lo que significa que cualquier salida por `std::process::exit` —y hay
/// varias: el Ctrl+C de la CLI, el `--pick` de la TUI, el `terminate:` de
/// macOS— tira justo las últimas líneas, que son las del fallo que alguien
/// está investigando. Estos binarios loguean a nivel INFO unas pocas líneas por
/// sesión; el coste de escribir a pelo no se mide, y a cambio desaparece toda
/// una clase de fallo.
///
/// `None` en vez de `Err`: un log es diagnóstico, y un diagnóstico que impide
/// arrancar es peor que no tenerlo.
fn file_layer<S>(dir: &Path, retain: usize, prefix: &str) -> Option<impl Layer<S>>
where
    S: tracing::Subscriber + for<'a> tracing_subscriber::registry::LookupSpan<'a>,
{
    create_dir_locked(dir, prefix).ok()?;
    let appender = tracing_appender::rolling::Builder::new()
        .rotation(tracing_appender::rolling::Rotation::DAILY)
        .filename_prefix(prefix)
        // `max_log_files(0)` haría que la poda borrase el fichero que está a
        // punto de escribir: uno es el mínimo que significa algo.
        .max_log_files(retain.max(1))
        .build(dir)
        .ok()?;
    Some(
        tracing_subscriber::fmt::layer()
            // Un fichero no es una terminal: los códigos de color lo ensucian.
            .with_ansi(false)
            .with_writer(appender),
    )
}

/// Cuántos ficheros rotados se conservan cuando la config no dice otra cosa.
/// Una semana: suficiente para que un fallo de ayer siga estando, poco para que
/// esto crezca sin que nadie lo mire.
const RETAIN_DEFAULT: usize = 7;

/// Dónde va el log: `[log] dir`, o `<state_dir>/logs`.
///
/// `None` = no hay directorio de estado (una CI pelada, un servicio sin `HOME`)
/// y por tanto no hay fichero. El caller degrada.
///
/// Un `[log] dir` RELATIVO se rehúsa y cae al default: se resolvería contra el
/// cwd, que en un gestor de ficheros es el directorio desde el que lo lanzaste
/// —a menudo un repositorio—, y el log acabaría dentro del árbol de trabajo de
/// cualquiera. Mismo criterio que ADR 0035 C1 aplica al directorio de config.
#[must_use]
pub fn log_dir(configured: Option<&Path>) -> Option<std::path::PathBuf> {
    match configured {
        Some(d) if d.is_absolute() => Some(d.to_path_buf()),
        Some(d) => {
            tracing::warn!(
                dir = %d.display(),
                "[log] dir es relativo y se ignora: el log iría a parar al cwd"
            );
            crate::dirs::state_dir().map(|s| s.join("logs"))
        }
        None => crate::dirs::state_dir().map(|s| s.join("logs")),
    }
}

/// Lo que `[log]` dice, tal y como los binarios lo tienen a mano.
///
/// Un struct y no dos parámetros sueltos porque los tres sitios que instalan
/// logging tienen que pasar LO MISMO, y dos `Option` en fila son dos
/// oportunidades de cruzarlos.
#[derive(Debug, Clone, Copy, Default)]
pub struct LogConfig<'a> {
    /// `[log] dir`. `None` = `<state_dir>/logs`.
    pub dir: Option<&'a Path>,
    /// `[log] retain`. `None` = una semana de ficheros.
    pub retain: Option<usize>,
    /// Prefijo del fichero rotado. `None` = `norte.log`.
    ///
    /// Existe porque dos procesos distintos NO deben rotar el mismo fichero:
    /// la ventana gráfica y el daemon pueden estar vivos a la vez, y la
    /// retención de uno podadaría los ficheros del otro.
    pub prefix: Option<&'a str>,
}

/// Instala el subscriber global: stderr MÁS el fichero rotatorio, con el cap de
/// seguridad. Para `norte-cli` y el daemon.
///
/// Idempotente y no-fatal: si ya hay un subscriber, no hace nada.
pub fn init(cfg: LogConfig<'_>) {
    let _ = init_with(true, cfg, None);
}

/// Como [`init_to_file`], y además un anillo en memoria que el frontend puede
/// pintar (`panel.log`).
///
/// `None` si ya había un subscriber instalado, porque entonces NADIE le
/// escribe al anillo. Devolverlo igualmente —como hacía la primera versión—
/// dejaba al panel enseñando «nada que enseñar con este filtro» para siempre,
/// que es justo la confusión que el panel existe para no crear: «no ha pasado
/// nada» tiene que distinguirse de «no está conectado», y con el `Option` la
/// interfaz puede decir la segunda.
///
/// El nivel del anillo se sube luego en caliente con
/// [`LogRing::raise_to`](crate::logring::LogRing::raise_to). Arranca en INFO:
/// un nivel verboso se paga aunque nadie mire.
#[must_use]
pub fn init_to_file_with_ring(cfg: LogConfig<'_>, cap: usize) -> Option<crate::logring::LogRing> {
    let ring = crate::logring::LogRing::new(cap);
    init_with(false, cfg, Some(&ring)).then_some(ring)
}

/// Como [`init`] pero SOLO al fichero. Para los frontends.
///
/// La TUI no instalaba subscriber ninguno, y lo decía en un comentario: un
/// `fmt` a stderr pelea con la pantalla alternativa, así que cada
/// `tracing::warn!` que saliera de ahí se descartaba mudo. La GUI tampoco
/// instalaba ninguno. Los dos frontends que un usuario ejecuta de verdad no
/// producían un solo diagnóstico; esto es lo que lo arregla, y sin escribir un
/// byte en una pantalla que están dibujando.
pub fn init_to_file(cfg: LogConfig<'_>) {
    let _ = init_with(false, cfg, None);
}

/// El montaje común. `stderr` decide si va también la capa de terminal.
///
/// **Los filtros son POR CAPA y ya no uno global**, y ese cambio es lo que hace
/// posible el panel de registro: con un `EnvFilter` sobre todo el registro, un
/// nivel INFO significa que los `DEBUG` no se emiten, y entonces ningún panel
/// puede enseñarlos después — filtrar en la ventana lo que nunca se registró es
/// imposible. Con filtros por capa, el fichero y el stderr conservan
/// exactamente el suyo (mismo [`filter_from`], mismo cap de `suppaftp`) y el
/// anillo lleva el propio, que además se cambia en caliente.
/// Devuelve si ESTE montaje fue el que se instaló: `false` significa que ya
/// había un subscriber, y entonces nada de lo que se monta aquí recibe nada.
fn init_with(stderr: bool, cfg: LogConfig<'_>, ring: Option<&crate::logring::LogRing>) -> bool {
    let file = match log_dir(cfg.dir) {
        Some(d) => file_layer(
            &d,
            cfg.retain.unwrap_or(RETAIN_DEFAULT),
            cfg.prefix.unwrap_or(LOG_PREFIX),
        ),
        None => None,
    }
    .map(|l| l.with_filter(filter_from(None)));
    let terminal = stderr.then(|| {
        tracing_subscriber::fmt::layer()
            .with_writer(std::io::stderr)
            .with_filter(filter_from(None))
    });
    let anillo = ring.map(crate::logring::ring_layer);
    tracing_subscriber::registry()
        .with(file)
        .with(terminal)
        .with(anillo)
        .try_init()
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use tracing::Level;
    use tracing_subscriber::Layer;
    use tracing_subscriber::layer::Context;

    /// Capa que registra (target, nivel) de cada evento que la ATRAVIESA (ya
    /// filtrado): lo que aquí llega es exactamente lo que el sink loguearía.
    struct Collect(Arc<Mutex<Vec<(String, Level)>>>);
    impl<S: tracing::Subscriber> Layer<S> for Collect {
        fn on_event(&self, event: &tracing::Event<'_>, _ctx: Context<'_, S>) {
            let m = event.metadata();
            self.0
                .lock()
                .expect("lock")
                .push((m.target().to_string(), *m.level()));
        }
    }

    #[test]
    fn suppaftp_trace_capped_even_with_trace_env() {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let subscriber = tracing_subscriber::registry()
            .with(Collect(Arc::clone(&seen)))
            .with(filter_from(Some("trace,suppaftp=trace")));

        tracing::subscriber::with_default(subscriber, || {
            tracing::trace!(target: "suppaftp", "PASS hunter2"); // debe CAERSE
            tracing::info!(target: "suppaftp", "conectado"); // pasa
            tracing::trace!(target: "otro", "visible"); // pasa (no capado)
        });

        let seen = seen.lock().expect("lock");
        // La password (evento TRACE de suppaftp) NUNCA atraviesa el filtro.
        assert!(
            !seen.contains(&("suppaftp".to_string(), Level::TRACE)),
            "suppaftp TRACE debe estar capado: {seen:?}"
        );
        // Pero info de suppaftp y trace de otros targets sí.
        assert!(seen.contains(&("suppaftp".to_string(), Level::INFO)));
        assert!(seen.contains(&("otro".to_string(), Level::TRACE)));
    }

    /// Los ficheros que hay en `dir`, con su contenido concatenado.
    fn volcado(dir: &std::path::Path) -> (usize, String) {
        let ficheros: Vec<std::path::PathBuf> = std::fs::read_dir(dir)
            .expect("listar")
            .map(|e| e.expect("entrada").path())
            .collect();
        let texto = ficheros
            .iter()
            .map(|f| std::fs::read_to_string(f).unwrap_or_default())
            .collect();
        (ficheros.len(), texto)
    }

    /// El appender escribe de verdad, en el directorio que se le da.
    #[test]
    fn el_log_aterriza_en_un_fichero() {
        let dir = tempfile::tempdir().expect("tmp");
        let layer = file_layer(dir.path(), 3, LOG_PREFIX).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(None));
        tracing::subscriber::with_default(sub, || {
            tracing::info!(target: "norte_core::prueba", "una linea");
        });

        let (cuantos, texto) = volcado(dir.path());
        assert_eq!(cuantos, 1, "un fichero de log");
        assert!(texto.contains("una linea"), "el evento está: {texto}");
    }

    /// **El cap de `suppaftp` cubre el FICHERO igual que cubre stderr.**
    ///
    /// Se filtra en el registry, antes de cualquier capa, así que debería
    /// seguirse de la arquitectura — y por eso mismo se comprueba: «debería
    /// seguirse» no es una prueba, y lo que está en juego es una contraseña en
    /// un fichero que PERSISTE, que es peor que una que pasó por una terminal
    /// (regla dura 10).
    #[test]
    fn la_password_de_ftp_no_llega_al_fichero() {
        let dir = tempfile::tempdir().expect("tmp");
        let layer = file_layer(dir.path(), 3, LOG_PREFIX).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(Some("trace,suppaftp=trace")));
        tracing::subscriber::with_default(sub, || {
            tracing::trace!(target: "suppaftp", "PASS hunter2");
            tracing::info!(target: "suppaftp", "conectado");
        });

        let (_, texto) = volcado(dir.path());
        assert!(
            !texto.contains("hunter2"),
            "la password llegó al fichero: {texto}"
        );
        assert!(texto.contains("conectado"), "y lo que sí pasa, pasa");
    }

    /// **El directorio del log es 0700, y los ficheros que caen dentro 0600.**
    ///
    /// Lo que el log guarda es lo mismo que guarda el journal —cada copia, cada
    /// borrado, cada host remoto al que te conectas— y el journal es 0600. Con
    /// la umask por defecto esto salía 0755/0644, o sea legible por cualquier
    /// cuenta local de la máquina.
    #[cfg(unix)]
    #[test]
    fn el_log_no_lo_puede_leer_cualquiera() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("logs");
        let layer = file_layer(&dir, 3, LOG_PREFIX).expect("appender");
        let sub = tracing_subscriber::registry()
            .with(layer)
            .with(filter_from(None));
        tracing::subscriber::with_default(sub, || {
            tracing::info!(target: "norte_core::prueba", "una linea");
        });

        let modo =
            |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        assert_eq!(
            modo(&dir),
            0o700,
            "el directorio, solo para su dueño: es lo que hace inalcanzable lo de dentro"
        );
    }

    /// Y el barrido aprieta lo que ya hubiera de días anteriores.
    ///
    /// Hace falta porque `tracing-appender` abre sus ficheros él, con la umask
    /// y sin dejar elegir modo: el de HOY sale 0644 y el de mañana también. Que
    /// eso no importe depende del 0700 del directorio, así que el barrido es lo
    /// que arregla el caso en el que el directorio fue laxo alguna vez —una
    /// instalación anterior a este arreglo, por ejemplo— y quedaron ficheros
    /// legibles dentro.
    #[cfg(unix)]
    #[test]
    fn el_barrido_cierra_los_ficheros_que_ya_estaban() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tmp");
        let dir = tmp.path().join("logs");
        std::fs::create_dir_all(&dir).expect("dir");
        let viejo = dir.join("norte.log.2026-08-01");
        std::fs::write(&viejo, b"de ayer").expect("fichero");
        std::fs::set_permissions(&viejo, std::fs::Permissions::from_mode(0o644)).expect("chmod");
        // Un fichero AJENO no se toca: este directorio es nuestro, pero el
        // barrido solo se mete con lo que lleva nuestro prefijo.
        let ajeno = dir.join("otra-cosa.txt");
        std::fs::write(&ajeno, b"ajeno").expect("fichero");
        std::fs::set_permissions(&ajeno, std::fs::Permissions::from_mode(0o644)).expect("chmod");

        create_dir_locked(&dir, LOG_PREFIX).expect("barrido");

        let modo =
            |p: &std::path::Path| std::fs::metadata(p).expect("stat").permissions().mode() & 0o777;
        assert_eq!(modo(&viejo), 0o600, "el log de ayer, cerrado");
        assert_eq!(modo(&ajeno), 0o644, "lo que no es un log, intacto");
    }

    /// Y NO degrada el directorio de estado que lo contiene.
    ///
    /// Éste es el que de verdad muerde: `init_to_file` es lo PRIMERO que toca
    /// `<state_dir>` en los dos frontends —antes del journal, antes de todo—,
    /// así que un `create_dir_all` sin modo creaba `<state_dir>` a 0755. El
    /// journal viene después con su `DirBuilder::mode(0o700)`, que sobre un
    /// directorio que YA existe no hace chmod ninguno: el 0755 se quedaba para
    /// siempre, enseñando el listado de `journal.db`, `lua-trust.toml` y el
    /// spool a cualquier cuenta local. En una instalación nueva, y en silencio.
    #[cfg(unix)]
    #[test]
    fn crear_el_log_no_afloja_el_directorio_de_estado() {
        use std::os::unix::fs::PermissionsExt as _;

        let tmp = tempfile::tempdir().expect("tmp");
        let state = tmp.path().join("state").join("norte");
        let _layer = file_layer::<tracing_subscriber::Registry>(&state.join("logs"), 3, LOG_PREFIX)
            .expect("appender");

        let modo = std::fs::metadata(&state)
            .expect("stat")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(modo, 0o700, "el padre creado de paso, también cerrado");
    }

    /// Un directorio que no se puede crear NO tumba el programa: se degrada.
    /// Un log es diagnóstico, y un diagnóstico que impide arrancar es peor que
    /// no tenerlo.
    #[test]
    fn un_directorio_imposible_degrada_en_vez_de_fallar() {
        let dir = tempfile::tempdir().expect("tmp");
        // Un FICHERO donde debería ir el directorio: `create_dir_all` falla.
        let ocupado = dir.path().join("ocupado");
        std::fs::write(&ocupado, b"no soy un directorio").expect("fichero");
        let capa = file_layer::<tracing_subscriber::Registry>(&ocupado, 3, LOG_PREFIX);
        assert!(capa.is_none(), "no se puede crear ahí, así que no hay capa");
    }
}
