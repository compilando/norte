//! CLI de humo de M0: `ls`, `cp`, `mv`, `rm` sobre el core embebido.
//!
//! Frontend SIN lógica de negocio (regla dura 7): todo pasa por `Engine`.
//! Strings hardcodeados a propósito: Fluent llega en M1 (deuda registrada).
#![forbid(unsafe_code)]

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use norte_core::{Engine, TransferOptions};
use norte_proto::SymlinkPolicy;

mod cmd;
mod doctor;
mod help;
mod paths;
mod task;
mod theme;

/// Ruta del binario de frontend a lanzar: el HERMANO de `exe` si existe,
/// si no el nombre pelado (que el `PATH` resolverá). Puro para poder
/// fijarlo en un test — el orden importa: un `norte` recién instalado tiene
/// que preferir el `norte-tui` de su propia tanda antes que uno más viejo
/// que ande antes en el `PATH`.
fn frontend_program(exe: Option<&std::path::Path>, bin: &str) -> PathBuf {
    exe.and_then(std::path::Path::parent)
        .map(|d| d.join(bin))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(bin))
}

/// Nombre del binario del frontend de TERMINAL.
///
/// Una constante y no un literal en el `match`: es la MISMA cadena que el
/// `[[bin]]` de `crates/norte-tui/Cargo.toml`, y un test las cruza
/// (`tui_lanza_el_binario_que_el_manifiesto_construye`). Sin ese cruce, el
/// nombre es un string que compila igual de bien esté bien o mal y falla en el
/// `exec`, con el usuario delante.
const TUI_BIN: &str = "ntc";

/// Localiza un binario HERMANO (hoy solo `ntc`) y le cede el
/// proceso. Busca primero JUNTO a este ejecutable —así un `norte` recién
/// instalado usa el `norte-tui` de la misma tanda, y no otro más viejo que
/// haya antes en el `PATH`— y si no está, deja que el `PATH` decida.
///
/// En unix hace `exec`: el frontend HEREDA el proceso (mismo pid, misma
/// terminal, mismas señales), así que Ctrl-C, el tamaño del terminal y el
/// código de salida se comportan como si se hubiera lanzado directamente,
/// sin un `norte` de más esperando en medio. En el resto de plataformas se
/// lanza como hijo y se propaga su código de salida.
fn exec_frontend(bin: &str, args: &[std::ffi::OsString]) -> anyhow::Result<ExitCode> {
    let programa = frontend_program(std::env::current_exe().ok().as_deref(), bin);
    let mut cmd = std::process::Command::new(&programa);
    cmd.args(args);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // `exec` solo VUELVE si falló: el error de abajo es el único camino.
        let e = cmd.exec();
        Err(anyhow::Error::new(e).context(format!(
            "no se pudo ejecutar `{bin}` ({}) — instálalo con `cargo install --path crates/{bin}`",
            programa.display()
        )))
    }
    #[cfg(not(unix))]
    {
        let estado = cmd.status().with_context(|| {
            format!(
                "no se pudo ejecutar `{bin}` ({}) — instálalo con `cargo install --path crates/{bin}`",
                programa.display()
            )
        })?;
        Ok(ExitCode::from(
            u8::try_from(estado.code().unwrap_or(1)).unwrap_or(1),
        ))
    }
}

/// Código de salida convencional para "interrumpido por SIGINT".
const EXIT_CANCELLED: u8 = 130;

#[derive(Parser)]
#[command(
    name = "norte",
    version = norte_frontend::version::VERSION_LINE,
    about = "file manager ortodoxo — CLI de humo (M0)",
    // H3g: `help` es NUESTRO subcomando (el corpus de ayuda, ADR 0040), no el
    // que clap genera para reimprimir su propio `--help`. Sin esto clap aborta
    // al construir el parser: «command name `help` is duplicated». La ayuda de
    // clap sigue estando donde siempre — `norte --help`, `norte <cmd> --help`;
    // lo que se pierde es `norte help <cmd>` como sinónimo de ese `--help`, y
    // ese nombre lo quiere la documentación del producto.
    disable_help_subcommand = true
)]
struct Cli {
    /// Opera contra el daemon (arrancándolo si hace falta) en vez del
    /// core embebido. Solo unix (ADR 0011).
    #[arg(long, global = true)]
    daemon: bool,
    /// Socket del daemon (default: `$XDG_RUNTIME_DIR/norte/daemon.sock`)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Lista un directorio
    Ls {
        /// Directorio a listar
        path: PathBuf,
        /// Salida JSON (paths en forma wire, lossless)
        #[arg(long)]
        json: bool,
        /// Atributos de provider a pedir por entrada, repetible (p. ej.
        /// `--attrs posix.mode`); el catálogo lo publica `fs.capabilities`
        #[arg(long = "attrs", value_name = "ID")]
        attrs: Vec<String>,
    },
    /// Copia archivo o directorio (recursivo), con progreso y Ctrl-C limpio
    Cp {
        /// Origen
        src: PathBuf,
        /// Destino EXACTO (si existe: conflicto, jamás sobrescribe)
        dst: PathBuf,
        /// Política de symlinks (ADR 0005)
        #[arg(long, value_enum, default_value = "preserve")]
        symlinks: SymlinksArg,
        /// Reanudar una copia interrumpida (deja/usa `.norte-partial`, ADR 0012)
        #[arg(long)]
        resume: bool,
    },
    /// Mueve/renombra, con progreso y Ctrl-C limpio
    Mv {
        /// Origen
        src: PathBuf,
        /// Destino exacto
        dst: PathBuf,
        /// Política de symlinks (ADR 0005; solo aplica al camino copy+delete)
        #[arg(long, value_enum, default_value = "preserve")]
        symlinks: SymlinksArg,
        /// Reanudar un movimiento interrumpido (camino copy+delete, ADR 0012)
        #[arg(long)]
        resume: bool,
    },
    /// Borra archivo o directorio (recursivo), con progreso y Ctrl-C limpio
    /// Borra PERMANENTE (banco de pruebas del engine; la papelera vive
    /// en el TUI — ADR 0009).
    Rm {
        /// Nodo a borrar
        path: PathBuf,
    },
    /// Crea UN directorio (sin `-p`: el padre debe existir; destino
    /// ocupado = conflicto) — #104
    Mkdir {
        /// Directorio a crear (el último segmento es el nombre nuevo)
        path: PathBuf,
    },
    /// Establece una conexión remota (por nombre de `connections.toml` o
    /// URL `sftp://…`/`ftp://…`), con el flujo TOFU interactivo (fase 6e)
    Connect {
        /// Nombre de la conexión o URL remota
        target: String,
    },
    /// Daemon JSON-RPC sobre UDS (ADR 0011; solo unix en M2)
    #[cfg(unix)]
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Sirve MCP por stdio para agentes (Claude Code, Codex…): conecta al
    /// daemon como sesión de agente (M3-4, ADR 0024)
    #[cfg(unix)]
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// Gobernanza de agentes desde el lado humano (M3-4)
    #[cfg(unix)]
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
    },
    /// Deshace la sesión completa de un agente en LIFO estricto (M3-4)
    #[cfg(unix)]
    Undo {
        /// Sesión de agente (la de `--session` del puente MCP)
        session: String,
    },
    /// Ejecuta comandos de plugins ya aprobados+activados (M4-P4)
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// Índice de búsqueda: `index build <path>` / `index query <path> <texto>` (M4)
    Index {
        #[command(subcommand)]
        cmd: IndexCmd,
    },
    /// Sugerencias de IA (M4, ADR 0031). Opt-in por `[ai]` de norte.toml;
    /// SIEMPRE produce un plan REVISABLE que confirmas antes de aplicar
    Ai {
        #[command(subcommand)]
        cmd: AiCmd,
    },
    /// Barre staging `.norte-partial` huérfano de un directorio (#11, ADR
    /// 0012). NO recursivo, no toca archivos del usuario (reconoce el
    /// staging por su forma exacta). Solo en modo embebido
    Gc {
        /// Directorio a barrer
        path: PathBuf,
        /// Antigüedad mínima en horas (una reanudación EN CURSO no debe
        /// barrerse: usa un umbral holgado)
        #[arg(long, default_value_t = 24)]
        older_than_hours: u64,
    },
    /// Auditoría del journal (M3-5, ADR 0025): cadena + anclas + export.
    /// Requiere que NADIE lo tenga abierto: ni el daemon, ni un frontend
    /// embebido que ya haya mutado algo. La DB se abre en solo-lectura, pero
    /// quien la tiene la bloquea en exclusiva
    Audit {
        #[command(subcommand)]
        cmd: AuditCmd,
    },
    /// Abre el frontend de TERMINAL (`norte-tui`) en este directorio (o en
    /// el que se pase). Los argumentos viajan tal cual al binario
    /// (`norte tui --help` los explica)
    #[command(disable_help_flag = true)]
    Tui {
        /// Argumentos para `norte-tui`, verbatim
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Diagnósticos de solo lectura sobre capas de config y keymaps (H2)
    Doctor {
        /// Salida JSON en vez de texto para humanos
        #[arg(long)]
        json: bool,
    },
    /// Dice DÓNDE está cada fichero que norte lee o escribe: las capas de
    /// config, y de la resuelta el `norte.toml`, las teclas, las conexiones,
    /// los secretos, el journal, el índice, los logs y el socket del daemon
    Paths {
        /// Salida JSON en vez de texto para humanos
        #[arg(long)]
        json: bool,
    },
    /// Documentación de norte: índice, una página, búsqueda, o la hoja de
    /// teclas EFECTIVAS. Embebida — no necesita daemon (H3g)
    Help {
        /// Página a mostrar (`--list` las enumera). `keys` es la hoja de
        /// teclado, generada del keymap efectivo
        topic: Option<String>,
        /// Enumera las páginas: id y título
        #[arg(long)]
        list: bool,
        /// Busca en títulos, etiquetas, comandos y cuerpo
        #[arg(long, value_name = "TEXTO")]
        search: Option<String>,
        /// Salida JSON (para agentes y goldens)
        #[arg(long)]
        json: bool,
    },
    /// Temas: importa uno de VS Code a `<config>/themes/`. Sin engine ni daemon
    Theme {
        #[command(subcommand)]
        cmd: theme::ThemeCmd,
    },
    /// Prints the cd-on-quit wrapper for a shell, to be `eval`ed (bash/zsh)
    /// or `source`d (fish) from the shell's rc file (S3, shell-integration)
    ShellInit {
        /// bash, zsh or fish
        shell: String,
    },
    /// Compara dos árboles y contesta en el CÓDIGO DE SALIDA (0 iguales,
    /// 1 difieren, 2 no se pudo saber)
    Compare {
        /// Árbol izquierdo
        a: PathBuf,
        /// Árbol derecho
        b: PathBuf,
        /// Una fila por línea, en JSON, sin traducir
        #[arg(long)]
        json: bool,
        /// Criterios de comparación (por defecto los del wire)
        #[arg(long, value_delimiter = ',')]
        criteria: Vec<String>,
        /// Profundidad máxima del recorrido
        #[arg(long)]
        max_depth: Option<u32>,
        /// Tolerancia de mtime en milisegundos
        #[arg(long)]
        mtime_tolerance_ms: Option<u32>,
    },
    /// Sincroniza un árbol sobre otro en UN sentido. Planifica, enseña el
    /// plan, y pregunta antes de aplicar. Contesta en el CÓDIGO DE SALIDA
    /// (0 no había nada que hacer, 1 se aplicó —o con `--dry-run` se enseñó—,
    /// 2 no ocurrió: ni se planificó, ni se aprobó, ni se pudo aplicar)
    Sync {
        /// De dónde se lee
        source: PathBuf,
        /// Dónde se escribe
        dest: PathBuf,
        /// `update` copia lo que falta o cambió; `mirror` además BORRA lo que
        /// sobra en el destino
        #[arg(long, value_enum)]
        mode: SyncModeArg,
        /// Enseña el plan y para: no aplica nada
        #[arg(long)]
        dry_run: bool,
        /// Aplica sin preguntar (el plan se imprime igual)
        #[arg(long)]
        yes: bool,
        /// Criterios de comparación
        #[arg(long, value_delimiter = ',')]
        criteria: Vec<String>,
        /// Tolerancia de mtime en milisegundos
        #[arg(long)]
        mtime_tolerance_ms: Option<u32>,
    },
}

/// La ortografía de un modo de sincronización que ve el CLI. Distinta del
/// `SyncMode` del wire a propósito (regla dura 8 implícita en la spec del
/// plan): la del CLI es presentación, la del wire es un contrato, y no hace
/// falta que ambas cambien juntas.
#[derive(Clone, Copy, clap::ValueEnum)]
enum SyncModeArg {
    /// Copia lo que falta o cambió; nunca borra.
    Update,
    /// `Update` más borrar del destino lo que el origen no tiene.
    Mirror,
}

impl From<SyncModeArg> for norte_proto::methods::SyncMode {
    fn from(mode: SyncModeArg) -> Self {
        match mode {
            SyncModeArg::Update => Self::Update,
            SyncModeArg::Mirror => Self::Mirror,
        }
    }
}

/// Las opciones de `norte sync` tal como llegan de `clap`, agrupadas para no
/// pasarle a `sync_cmd` sus ocho campos sueltos (`clippy::too_many_arguments`
/// corta en 7, y `source`/`dest`/`backend` ya ocupan los otros tres).
struct SyncCliOpts<'a> {
    mode: SyncModeArg,
    dry_run: bool,
    yes: bool,
    criteria: &'a [String],
    mtime_tolerance_ms: Option<u32>,
}

/// Subcomandos de IA (M4-A2).
#[derive(Subcommand)]
enum AiCmd {
    /// Propone un rename por lote de los archivos de un dir según una
    /// instrucción; imprime el plan y pide confirmación antes de aplicar
    Rename {
        /// Directorio cuyos archivos renombrar
        dir: PathBuf,
        /// Instrucción en lenguaje natural (p. ej. "a minúsculas")
        instruction: String,
        /// Aplica sin preguntar (por defecto se confirma — es revisable)
        #[arg(long)]
        yes: bool,
    },
}

/// Subcomandos del índice de búsqueda (M4, ADR 0034).
#[derive(Subcommand)]
enum IndexCmd {
    /// (Re)construye el índice de un subárbol (Task cancelable)
    Build {
        /// Raíz a indexar (path local o URL remota)
        path: PathBuf,
    },
    /// Busca en el índice de un root por texto
    Query {
        /// Raíz cuyo índice consultar
        path: PathBuf,
        /// Texto libre (prefijo-AND de los términos)
        text: String,
        /// Tope de resultados
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Genera embeddings del root ya indexado (requiere `[ai]` + `embed_provider`)
    Embed {
        /// Raíz ya indexada con `index build`
        path: PathBuf,
    },
    /// Búsqueda semántica; sin --root busca en todos los roots
    Semantic {
        /// Consulta en lenguaje natural
        text: String,
        /// Raíz cuyo índice consultar (por defecto, todos)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Tope de resultados (el server recorta a su máximo)
        #[arg(long, default_value_t = 20)]
        k: u32,
    },
}

/// Subcomandos de auditoría (M3-5).
#[derive(Subcommand)]
enum AuditCmd {
    /// Verifica el hash-chain (cita la primera rotura) y las anclas HMAC.
    /// SIN anclas el veredicto es FALLO (su ausencia es indistinguible de
    /// un borrado hostil) salvo opt-out explícito
    Verify {
        /// Acepta un journal sin fichero de anclas (primer uso)
        #[arg(long)]
        allow_no_anchors: bool,
    },
    /// Exporta el journal a STDOUT
    Export {
        /// Formato de salida
        #[arg(long, value_enum, default_value_t = AuditFormat::Jsonl)]
        format: AuditFormat,
    },
    /// Ancla el head actual de la cadena (HMAC con clave del keyring) y
    /// escribe la línea a STDOUT — guárdala TAMBIÉN fuera de esta máquina:
    /// la copia externa es lo que hace detectable un recorte del fichero
    Anchor,
}

/// Formato del export de auditoría.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum AuditFormat {
    /// Una línea JSON por entrada (estable, para máquinas)
    Jsonl,
    /// CSV RFC 4180 con fórmulas neutralizadas (para humanos)
    Csv,
}

/// Subcomandos de plugins.
#[derive(Subcommand)]
enum PluginCmd {
    /// Ejecuta un comando de un plugin y escribe su salida a STDOUT
    Run {
        /// Id del plugin (reverse-DNS, p. ej. `org.norte.demo`)
        id: String,
        /// Comando declarado por el plugin
        command: String,
        /// Argumento del comando (ausente = "")
        #[arg(default_value = "")]
        arg: String,
    },
    /// Instala un plugin desde un directorio local (`plugin.toml` + `plugin.wasm`)
    Install {
        /// Directorio con el plugin
        path: std::path::PathBuf,
        /// Reemplaza uno ya instalado con el mismo id. RETIRA su consentimiento
        #[arg(long)]
        force: bool,
    },
    /// Desinstala un plugin por su id. RETIRA su consentimiento
    Uninstall {
        /// Id del plugin (reverse-DNS, p. ej. `org.norte.demo`)
        id: String,
    },
    /// Lista los plugins instalados con su estado (aprobado, activado) y capabilities
    List,
}

/// Subcomandos MCP.
#[cfg(unix)]
#[derive(Subcommand)]
enum McpCmd {
    /// Sirve MCP por stdio hasta EOF (arranca el daemon si hace falta)
    Serve {
        /// Id de sesión de agente (`[A-Za-z0-9._-]`, 1..=64)
        #[arg(long, default_value = "mcp")]
        session: String,
    },
}

/// Subcomandos de policy (lado humano).
#[cfg(unix)]
#[derive(Subcommand)]
enum PolicyCmd {
    /// Concede una petición de scope pendiente (el `request_id` lo imprime
    /// el agente al llamar a la tool `request_scope`)
    Grant {
        /// `request_id` devuelto por `request_scope`
        request_id: u64,
    },
}

/// Subcomandos del daemon.
#[cfg(unix)]
#[derive(Subcommand)]
enum DaemonCmd {
    /// Sirve en primer plano hasta shutdown (petición, SIGTERM/Ctrl-C o
    /// inactividad)
    Run {
        /// Path del socket (default: `$XDG_RUNTIME_DIR/norte/daemon.sock`)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Apagado tras N segundos sin clientes ni tasks (0 = nunca)
        #[arg(long, default_value_t = 300)]
        idle_timeout: u64,
    },
    /// Pide el apagado al daemon en marcha
    Stop {
        /// Path del socket (default: el mismo que run)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Cancela las tasks vivas en vez de esperarlas
        #[arg(long)]
        hard: bool,
        /// Relevo: avisa a los clientes de que VUELVAN (viene un daemon nuevo)
        ///
        /// Sin esto, parar el daemon les dice que no vuelvan — que es lo
        /// correcto cuando lo paras tú, y lo contrario de lo que hace falta
        /// cuando lo estás sustituyendo. Se rehúsa si hay tasks vivas.
        #[arg(long)]
        handover: bool,
    },
}

/// Política de symlinks de `cp`/`mv` (mapea 1:1 a la del protocolo).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum SymlinksArg {
    /// Copia el LINK tal cual (como `cp -a`)
    Preserve,
    /// Los symlinks no se copian
    Skip,
    /// Copia el CONTENIDO apuntado; los dir-symlinks se expanden como
    /// dirs reales (un ciclo de links aborta la operación)
    Follow,
}

/// `--resume` → política (opt-in; sin el flag, contrato de M1).
fn resume_policy(on: bool) -> norte_proto::ResumePolicy {
    if on {
        norte_proto::ResumePolicy::On
    } else {
        norte_proto::ResumePolicy::Off
    }
}

impl From<SymlinksArg> for SymlinkPolicy {
    fn from(a: SymlinksArg) -> Self {
        match a {
            SymlinksArg::Preserve => SymlinkPolicy::Preserve,
            SymlinksArg::Skip => SymlinkPolicy::Skip,
            SymlinksArg::Follow => SymlinkPolicy::Follow,
        }
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let rt = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(rt) => rt,
        Err(e) => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-runtime-error", &[("error", &e.to_string())])
            );
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(run(cli)) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("norte: {e:#}");
            ExitCode::FAILURE
        }
    }
}

/// El aviso de «esta sesión no queda registrada» a stderr, EN EL ACTO (#177).
///
/// Por `eprintln!` y no por el `tracing::warn!` que el core emitiría si nadie
/// instalara sink: el default de `logging::init` es INFO pero respeta
/// `RUST_LOG`, así que un `RUST_LOG=error norte mv a b` con un daemon vivo
/// movería el fichero, no registraría nada y no diría NADA. Es el mismo
/// criterio que `report_degradations` (ADR 0015 F, «nunca silencioso») y el que
/// tenía el `abrir_journal_embebido` que #177 borró.
///
/// Directo, sin canal: el aviso nace dentro de la mutación en curso y este
/// binario no tiene run loop donde drenarlo — que salga cuando ocurre es
/// justamente lo que se quiere.
struct AvisoDeJournalPorStderr;

impl norte_core::embedded::JournalWarningSink for AvisoDeJournalPorStderr {
    fn on_no_journal(&self, why: &norte_core::embedded::NoJournal) {
        aviso(&why.text());
    }

    /// Un `norte cp` hace una mutación y se muere, así que la recuperación de
    /// #179 aquí es casi teórica — pero un `norte ai rename --yes` de cuarenta
    /// ficheros dura lo bastante como para que el ocupante suelte a mitad, y
    /// entonces el aviso de arriba se quedó dicho sobre unos ficheros y no
    /// sobre los otros. Decirlo cuesta una línea.
    ///
    /// Por Fluent, a diferencia de su hermano: `NoJournal::text()` está
    /// documentado como la frase SIN traducir del log del operador, y esta no
    /// tiene esa excusa — es interfaz, y un `LANG=en` no puede leerla en
    /// castellano.
    fn on_journal_recovered(&self) {
        aviso(&norte_i18n::t("msg-journal-recovered"));
    }

    /// #203: lleva minutos ocupado y no hay daemon que lo explique. Frase
    /// propia, porque la de siempre («no queda registrado») es la que el
    /// usuario ya aprendió a ignorar — sale igual cuando no pasa nada.
    fn on_journal_squatted(&self) {
        aviso(&norte_i18n::t("msg-journal-squatted"));
    }
}

/// Una línea de aviso a stderr que NO puede tumbar la operación que la produjo.
///
/// `eprintln!` PANICA si stderr falla (cerrado, o lleno en un pipeline), y este
/// sink corre dentro del `on_mutation` de una mutación que ya se aplicó: ese
/// pánico haría fallar la Task de algo que funcionó, que es exactamente lo que
/// el rustdoc de `JournalWarningSink` prohíbe. Además se emite con locks del
/// engine tomados.
fn aviso(frase: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "aviso: {frase}");
}

#[expect(clippy::too_many_lines, reason = "un brazo por subcomando")]
async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    // Tracing con el cap de seguridad `suppaftp=info` (issue #43, regla 10):
    // sin esto un `RUST_LOG=trace` volcaría `PASS <password>` de suppaftp.
    //
    // El guard SE SOSTIENE hasta el final de `run` (roadmap ítem 9): el writer
    // del fichero es no bloqueante y su hilo vacía la cola al soltarlo, así que
    // dejarlo caer aquí tiraría justo las últimas líneas — las del fallo que
    // alguien está diagnosticando.
    // `[log]` sale de la config, así que se carga ANTES del subscriber. Es una
    // lectura de ficheros pequeños y sin ella la CLI y el daemon escribirían en
    // un sitio distinto del que escriben los frontends — con `norte doctor`
    // señalando uno de los dos, que es peor que no señalar ninguno.
    let cfg_log = norte_config::load(&norte_config::standard_layers()).ok();
    let log_cfg = norte_core::logging::LogConfig {
        dir: cfg_log.as_ref().and_then(|c| c.log.dir.as_deref()),
        retain: cfg_log.as_ref().and_then(|c| c.log.retain),
        // El fichero compartido: es el que lee `norte doctor`.
        prefix: None,
        format: cfg_log.as_ref().map(|c| c.log.format).unwrap_or_default(),
    };
    // `norte daemon run` —y solo él— monta además un anillo en memoria (#328,
    // ADR 0092): es el registro que `log.tail` sirve a un frontend que vive en
    // otro proceso, y sin él la ventana pinta el anillo del proceso
    // equivocado (#326). Todo lo demás sigue con `init` a secas, porque no
    // tiene a quién enseñárselo y pagaría dos mil líneas de memoria por nadie
    // — y eso incluye `norte daemon stop`, que es un cliente que manda una
    // petición y se muere.
    //
    // `init_with_ring` y no `init_to_file_with_ring`: este camino ya montaba
    // `init`, o sea CON stderr, así que la otra función no habría añadido un
    // anillo sino QUITADO una capa. Quien arranca `norte daemon run` en una
    // terminal para ver por qué no levanta dejaría de leer nada.
    #[cfg(unix)]
    let anillo_de_registro = if matches!(
        cli.cmd,
        Cmd::Daemon {
            cmd: DaemonCmd::Run { .. }
        }
    ) {
        norte_core::logging::init_with_ring(log_cfg, norte_config::logring::RING_DEFAULT)
    } else {
        norte_core::logging::init(log_cfg);
        None
    };
    #[cfg(not(unix))]
    norte_core::logging::init(log_cfg);

    // El daemon construye SU PROPIO engine (con journal+policy, M3-4): el
    // embebido de abajo es solo para el resto de subcomandos.
    #[cfg(unix)]
    if let Cmd::Daemon { cmd } = cli.cmd {
        return cmd::daemon::daemon_cmd(cmd, anillo_de_registro).await;
    }
    // MCP/policy/undo hablan al daemon directamente como cliente (no van por
    // el Backend embebido): el daemon es el dueño del journal y la policy.
    #[cfg(unix)]
    match cli.cmd {
        Cmd::Mcp { cmd } => return cmd::daemon::mcp_cmd(cmd, cli.socket).await,
        Cmd::Policy { cmd } => return cmd::daemon::policy_cmd(cmd, cli.socket).await,
        Cmd::Undo { session } => return cmd::daemon::undo_cmd(&session, cli.socket).await,
        Cmd::Ai { cmd } => return cmd::ai::ai_cmd(cmd).await,
        _ => {}
    }
    // Audit lee la DB del journal directamente (solo-lectura, daemon parado).
    if let Cmd::Audit { cmd } = cli.cmd {
        return cmd::audit::audit_cmd(cmd).await;
    }
    // Un frontend es un proceso APARTE (el TUI toma la terminal; el gráfico,
    // cuando lo haya, abrirá ventana): este CLI solo lo localiza y le cede el
    // proceso — nada de engine ni daemon aquí.
    if let Cmd::Tui { ref args } = cli.cmd {
        return exec_frontend(TUI_BIN, args);
    }
    // Doctor es solo-lectura sobre config/keymaps (H2): ni engine ni daemon.
    if let Cmd::Doctor { json } = cli.cmd {
        return cmd::entorno::doctor_cmd(json).await;
    }
    // `paths` resuelve rutas y las statea, nada más: mismo sitio por el mismo
    // motivo. Y ANTES de construir engine: la pregunta «dónde está mi config»
    // se hace justo cuando algo de eso está roto.
    if let Cmd::Paths { json } = cli.cmd {
        return cmd::entorno::paths_cmd(json, cli.socket.clone()).await;
    }
    // La ayuda es el corpus EMBEBIDO más el keymap del usuario (H3g): ni
    // engine, ni daemon, ni red. Va aquí arriba por eso — construir un engine
    // para imprimir documentación sería trabajo que el lector paga sin verlo.
    if let Cmd::Help {
        ref topic,
        list,
        ref search,
        json,
    } = cli.cmd
    {
        return Ok(help::run(topic.as_deref(), list, search.as_deref(), json));
    }
    // `shell-init` just prints a constant string picked by name (S3): no
    // engine, no daemon, no config — the same reasoning as `Help` above.
    if let Cmd::ShellInit { ref shell } = cli.cmd {
        return Ok(cmd::entorno::shell_init_cmd(shell));
    }
    // `theme import` lee un JSON y escribe un TOML en el dir de config: ni
    // engine ni daemon, y todo síncrono, así que a `spawn_blocking` (regla 2).
    if let Cmd::Theme { ref cmd } = cli.cmd {
        let cmd = cmd.clone();
        return tokio::task::spawn_blocking(move || theme::run(&cmd)).await?;
    }

    // Índice de búsqueda (M4, ADR 0034): el MISMO fichero que el daemon
    // (config_dir/index.db), así `norte index build` en embebido persiste y una
    // query posterior lo lee. SOLO se abre en modo embebido: con `--daemon` el
    // dueño del índice es el daemon (se accede por RPC), y abrirlo aquí solo
    // arriesgaría contención de escritura. Si no abre, se sigue sin él.
    let mut avisos = Vec::new();
    let engine = if cli.daemon {
        Engine::new()
    } else {
        // #167: regla dura 4 — lo que MUTA el árbol se registra. El journal es
        // el MISMO fichero que abriría el daemon (`config_dir()/journal.db`),
        // y esa igualdad es el punto: `norte undo` contra el daemon tiene que
        // ver lo que hizo un `norte mv` embebido. Si otro proceso tiene el
        // lock, se sigue sin registro y con aviso — ver `norte_core::embedded`.
        //
        // Se arma para TODOS los subcomandos, incluido `norte ls`, y eso no le
        // quita el journal a nadie: desde #177 el fichero no se abre hasta la
        // primera mutación. Aquí había una lista de subcomandos «que mutan»
        // mantenida a mano —con `norte ai rename` ya fuera de ella, abriendo su
        // journal por su cuenta— y es la que ese cambio hizo innecesaria.
        let dir = norte_core::connect::config_dir();
        let base = norte_core::embedded::engine_in(&dir);
        norte_core::equipo::con_indice(base, &dir, &mut avisos).await
    };
    if !cli.daemon {
        // Proveedor local, conector y —solo para los dos comandos que los
        // usan— los embeddings: lo que lleva todo engine
        // (`norte_core::equipo`). Cargar `[ai]` resuelve secretos, y eso
        // jamás lo paga un `ls`. Con `--daemon` el dueño de todo esto es el
        // daemon.
        let ia = norte_core::equipo::Ia {
            renombrado: false,
            embeddings: matches!(
                cli.cmd,
                Cmd::Index {
                    cmd: IndexCmd::Embed { .. } | IndexCmd::Semantic { .. }
                }
            ),
        };
        let dir = norte_core::connect::config_dir();
        avisos.extend(norte_core::equipo::equipar(&engine, &dir, ia).await.avisos);
        // `[archive]` también en embebido: sin esto un `norte ls` dentro de un
        // zip usaba los límites por defecto aunque `norte.toml` fijara otros.
        // Roto, aquí es un aviso; en el daemon, un error de arranque.
        if let Err(e) = norte_core::archive_config::aplicar(&engine).await {
            avisos.push(norte_core::equipo::Aviso::ArchivoInvalido(e.to_string()));
        }
    }
    for aviso in &avisos {
        eprintln!("{}", cmd::daemon::texto_del_aviso(aviso));
    }

    // #177: si esta sesión acaba mutando sin journal, se dice a stderr y en el
    // acto. No-op sobre el engine de `--daemon` (ese no puede quedarse sin
    // journal: el dueño es el daemon, al otro lado del socket).
    engine.set_journal_warning_sink(Arc::new(AvisoDeJournalPorStderr));
    // `sync.plan`/`sync.apply` retienen el plan aprobado en un spool en disco
    // (ADR 0049); sin instalarlo el brazo embebido contesta `Unsupported` —
    // ver la rustdoc de `Backend::is_journalled`. Solo se instala para `norte
    // sync`, con el mismo criterio LAZY que `[ai]` arriba: los demás
    // subcomandos no lo necesitan, y ningún barrido hace falta aquí (a
    // diferencia del arranque del daemon) porque el TTL de cada plan se
    // reapa solo, en `Spool::open`/`Spool::create`.
    if !cli.daemon && matches!(cli.cmd, Cmd::Sync { .. }) {
        engine.set_spool(norte_core::sync::Spool::new(
            norte_core::connect::config_dir(),
        ));
    }
    let mut backend = cmd::daemon::make_backend(engine, cli.daemon, cli.socket.clone()).await?;
    // #44: toma el canal de avisos de degradación ANTES de correr el comando
    // (en embebido esto INSTALA el observer, que dispara síncrono dentro del
    // establecimiento; en remoto toma el receptor del pump del daemon). Se
    // drena a stderr tras el comando — "nunca silencioso" (ADR 0015 F). En
    // remoto es best-effort: el pump es concurrente y el aviso también queda
    // en el `tracing::warn!` del daemon.
    let mut degraded = backend.take_degraded();
    let result = match cli.cmd {
        Cmd::Ls { path, json, attrs } => cmd::ls::ls(&backend, &path, json, &attrs).await,
        Cmd::Compare {
            a,
            b,
            json,
            criteria,
            max_depth,
            mtime_tolerance_ms,
        } => {
            // Un `Err` de estos dos comandos NO puede salir por el
            // `ExitCode::FAILURE` de `main`: ver `codigo_de_no_se_pudo`.
            Ok(cmd::compare::compare_cmd(
                &backend,
                &a,
                &b,
                json,
                &criteria,
                max_depth,
                mtime_tolerance_ms,
            )
            .await
            .unwrap_or_else(|e| cmd::compare::codigo_de_no_se_pudo(&e)))
        }
        Cmd::Sync {
            source,
            dest,
            mode,
            dry_run,
            yes,
            criteria,
            mtime_tolerance_ms,
        } => {
            // Mismo motivo que en `Cmd::Compare`: aquí el 1 significa «se
            // aplicó», así que ningún error puede compartirlo.
            Ok(cmd::sync::sync_cmd(
                &backend,
                &source,
                &dest,
                SyncCliOpts {
                    mode,
                    dry_run,
                    yes,
                    criteria: &criteria,
                    mtime_tolerance_ms,
                },
            )
            .await
            .unwrap_or_else(|e| cmd::compare::codigo_de_no_se_pudo(&e)))
        }
        Cmd::Connect { target } => cmd::connect::connect_cmd(&backend, &target, cli.daemon).await,
        Cmd::Cp {
            src,
            dst,
            symlinks,
            resume,
        } => {
            let (from, to) = (cmd::connect::vpath(&src)?, cmd::connect::vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                resume: resume_policy(resume),
                ..TransferOptions::default()
            };
            let task = match backend.copy(&from, &to, opts).await {
                // Primer contacto TOFU: confirmar y reintentar UNA vez.
                Err(e) if cmd::connect::tofu_confirm(&backend, &e).await? => {
                    backend.copy(&from, &to, opts).await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-copy"))?;
            Ok(task::run_task(task, true).await)
        }
        Cmd::Mv {
            src,
            dst,
            symlinks,
            resume,
        } => {
            let (from, to) = (cmd::connect::vpath(&src)?, cmd::connect::vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                resume: resume_policy(resume),
                ..TransferOptions::default()
            };
            let task = match backend.move_(&from, &to, opts).await {
                Err(e) if cmd::connect::tofu_confirm(&backend, &e).await? => {
                    backend.move_(&from, &to, opts).await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-move"))?;
            Ok(task::run_task(task, false).await)
        }
        Cmd::Gc {
            path,
            older_than_hours,
        } => cmd::ai::gc_cmd(&backend, cli.daemon, &path, older_than_hours).await,
        Cmd::Rm { path } => {
            let target = cmd::connect::vpath(&path)?;
            let task = match backend
                .delete(&target, norte_proto::DeleteMode::Permanent)
                .await
            {
                Err(e) if cmd::connect::tofu_confirm(&backend, &e).await? => {
                    backend
                        .delete(&target, norte_proto::DeleteMode::Permanent)
                        .await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-delete"))?;
            Ok(task::run_task(task, false).await)
        }
        Cmd::Mkdir { path } => {
            let target = cmd::connect::vpath(&path)?;
            let task = match backend.mkdir(&target).await {
                Err(e) if cmd::connect::tofu_confirm(&backend, &e).await? => {
                    backend.mkdir(&target).await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-mkdir"))?;
            Ok(task::run_task(task, false).await)
        }
        Cmd::Plugin { cmd } => cmd::plugin::plugin_cmd(&backend, cmd, cli.socket).await,
        Cmd::Index { cmd } => cmd::index::index_cmd(&backend, cmd).await,
        Cmd::Audit { .. }
        | Cmd::Ai { .. }
        | Cmd::Doctor { .. }
        | Cmd::Paths { .. }
        | Cmd::Help { .. }
        | Cmd::ShellInit { .. }
        | Cmd::Theme { .. }
        | Cmd::Tui { .. } => unreachable!("manejado arriba"),
        #[cfg(unix)]
        Cmd::Daemon { .. } | Cmd::Mcp { .. } | Cmd::Policy { .. } | Cmd::Undo { .. } => {
            unreachable!("manejado arriba")
        }
    };
    report_degradations(&mut degraded);
    result
}

/// Drena a stderr los avisos de degradación TLS acumulados (#44) — "nunca
/// silencioso" (ADR 0015 F). Best-effort en modo daemon (el pump es concurrente
/// y el aviso también queda en el `tracing::warn!` del daemon).
fn report_degradations(
    degraded: &mut Option<
        tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>,
    >,
) {
    if let Some(rx) = degraded.as_mut() {
        while let Ok(d) = rx.try_recv() {
            eprintln!(
                "{}",
                norte_i18n::ta(
                    "cli-connection-degraded",
                    &[("scheme", d.scheme.as_str()), ("host", d.host.as_str())],
                )
            );
        }
    }
}

#[cfg(test)]
mod frontend_tests {
    use super::frontend_program;

    /// El CLI lanza el frontend por NOMBRE DE BINARIO, así que el nombre que
    /// pasa tiene que ser el que el manifiesto construye. Un string que no
    /// corresponde a ningún binario compila igual de bien y falla en el
    /// `exec`, con el usuario delante: `norte tui` deja de funcionar y nada lo
    /// dice antes.
    ///
    /// El nombre esperado se LEE del `Cargo.toml` del crate hermano, no se
    /// escribe aquí: una constante en el test se renombraría con el mismo
    /// buscar-y-reemplazar que rompería el código, y entonces el test
    /// acompañaría al defecto en vez de cazarlo.
    #[test]
    fn tui_lanza_el_binario_que_el_manifiesto_construye() {
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../norte-tui/Cargo.toml");
        let toml = std::fs::read_to_string(&manifest).expect("el manifiesto del TUI");
        let esperado = toml
            .split("[[bin]]")
            .nth(1)
            .and_then(|s| s.lines().find_map(|l| l.trim().strip_prefix("name = ")))
            .map(|n| n.trim_matches('"').to_owned())
            .expect("el manifiesto declara [[bin]] name");
        assert_eq!(
            super::TUI_BIN,
            esperado,
            "el CLI lanza `{}` y el manifiesto construye `{esperado}`",
            super::TUI_BIN
        );
    }

    /// El hermano de al lado GANA al `PATH`: `norte` y `norte-tui` se
    /// instalan juntos, y mezclarlos con otra tanda es justo el fallo que
    /// costó una sesión de depuración (un binario de julio leyendo una
    /// config de agosto).
    #[test]
    fn prefiere_el_binario_hermano_y_si_no_cae_al_path() {
        let d = tempfile::tempdir().expect("tempdir");
        let exe = d.path().join("norte");
        std::fs::write(&exe, b"#!/bin/true\n").expect("write");
        // Sin hermano todavía: nombre pelado para que resuelva el PATH.
        assert_eq!(
            frontend_program(Some(&exe), "norte-tui"),
            std::path::PathBuf::from("norte-tui")
        );
        // Con hermano: ruta absoluta a ESE.
        let hermano = d.path().join("norte-tui");
        std::fs::write(&hermano, b"#!/bin/true\n").expect("write");
        assert_eq!(frontend_program(Some(&exe), "norte-tui"), hermano);
        // Sin saber dónde estamos: el PATH decide.
        assert_eq!(
            frontend_program(None, "otro-frontend"),
            std::path::PathBuf::from("otro-frontend")
        );
    }
}
