//! M0 smoke-test CLI: `ls`, `cp`, `mv`, `rm` over the embedded core.
//!
//! Frontend WITHOUT business logic (hard rule 7): everything goes through
//! `Engine`. Strings are hardcoded on purpose: Fluent arrives in M1
//! (recorded debt).
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

/// Path of the frontend binary to launch: `exe`'s SIBLING if it exists,
/// otherwise the bare name (which `PATH` will resolve). Pure so it can be
/// fixed in a test — order matters: a freshly installed `norte` has to
/// prefer the `norte-tui` from its own batch over an older one that comes
/// earlier in `PATH`.
fn frontend_program(exe: Option<&std::path::Path>, bin: &str) -> PathBuf {
    exe.and_then(std::path::Path::parent)
        .map(|d| d.join(bin))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| PathBuf::from(bin))
}

/// Name of the TERMINAL frontend's binary.
///
/// A constant and not a literal in the `match`: it is the SAME string as
/// `crates/norte-tui/Cargo.toml`'s `[[bin]]`, and a test cross-checks them
/// (`tui_launches_the_binary_the_manifest_builds`). Without that
/// cross-check, the name is a string that compiles equally well whether
/// right or wrong and fails at `exec`, with the user watching.
const TUI_BIN: &str = "ntc";

/// Locates a SIBLING binary (today only `ntc`) and hands it the process.
/// Looks first NEXT TO this executable — so a freshly installed `norte`
/// uses the `norte-tui` from the same batch, and not an older one earlier
/// in `PATH` — and if it is not there, lets `PATH` decide.
///
/// On unix it does `exec`: the frontend INHERITS the process (same pid,
/// same terminal, same signals), so Ctrl-C, the terminal size and the
/// exit code behave as if it had been launched directly, with no extra
/// `norte` waiting in between. On other platforms it is launched as a
/// child and its exit code is propagated.
fn exec_frontend(bin: &str, args: &[std::ffi::OsString]) -> anyhow::Result<ExitCode> {
    let program = frontend_program(std::env::current_exe().ok().as_deref(), bin);
    let mut cmd = std::process::Command::new(&program);
    cmd.args(args);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        // `exec` only RETURNS if it failed: the error below is the only path.
        let e = cmd.exec();
        Err(anyhow::Error::new(e).context(format!(
            "no se pudo ejecutar `{bin}` ({}) — instálalo con `cargo install --path crates/{bin}`",
            program.display()
        )))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().with_context(|| {
            format!(
                "no se pudo ejecutar `{bin}` ({}) — instálalo con `cargo install --path crates/{bin}`",
                program.display()
            )
        })?;
        Ok(ExitCode::from(
            u8::try_from(status.code().unwrap_or(1)).unwrap_or(1),
        ))
    }
}

/// Conventional exit code for "interrupted by SIGINT".
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

/// `norte sync`'s options as they arrive from `clap`, grouped so as not to
/// pass `sync_cmd` its eight loose fields (`clippy::too_many_arguments`
/// cuts off at 7, and `source`/`dest`/`backend` already take up the other
/// three).
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

/// `--resume` → policy (opt-in; without the flag, M1's contract).
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

/// The "this session is not being recorded" warning to stderr, ON THE SPOT
/// (#177).
///
/// Via `eprintln!` and not the `tracing::warn!` the core would emit if
/// nobody installed a sink: `logging::init`'s default is INFO but it
/// respects `RUST_LOG`, so a `RUST_LOG=error norte mv a b` with a live
/// daemon would move the file, record nothing and say NOTHING. It is the
/// same criterion as `report_degradations` (ADR 0015 F, "never silent")
/// and the one the `abrir_journal_embebido` that #177 deleted used to
/// have.
///
/// Direct, no channel: the warning is born inside the mutation in
/// progress and this binary has no run loop to drain it from — that it
/// comes out as it happens is exactly what is wanted.
struct JournalWarningStderr;

impl norte_core::embedded::JournalWarningSink for JournalWarningStderr {
    fn on_no_journal(&self, why: &norte_core::embedded::NoJournal) {
        warn_line(&why.text());
    }

    /// A `norte cp` makes one mutation and dies, so #179's recovery here is
    /// almost theoretical — but a `norte ai rename --yes` over forty files
    /// takes long enough for the occupant to let go halfway through, and
    /// then the warning above was said about some files and not the
    /// others. Saying so costs one line.
    ///
    /// Via Fluent, unlike its sibling: `NoJournal::text()` is documented
    /// as the operator log's UNTRANSLATED sentence, and this one has no
    /// such excuse — it is interface, and a `LANG=en` cannot read it in
    /// Spanish.
    fn on_journal_recovered(&self) {
        warn_line(&norte_i18n::t("msg-journal-recovered"));
    }

    /// #203: it has been busy for minutes and there is no daemon to
    /// explain it. Its own sentence, because the usual one ("not being
    /// recorded") is the one the user already learned to ignore — it
    /// comes out the same when nothing is wrong.
    fn on_journal_squatted(&self) {
        warn_line(&norte_i18n::t("msg-journal-squatted"));
    }
}

/// A warning line to stderr that must NOT be able to bring down the
/// operation that produced it.
///
/// `eprintln!` PANICS if stderr fails (closed, or full in a pipeline), and
/// this sink runs inside a mutation's `on_mutation` that has ALREADY been
/// applied: that panic would fail the Task for something that worked,
/// which is exactly what `JournalWarningSink`'s rustdoc forbids. It is
/// also emitted with engine locks held.
fn warn_line(phrase: &str) {
    use std::io::Write as _;
    let _ = writeln!(std::io::stderr(), "aviso: {phrase}");
}

#[expect(clippy::too_many_lines, reason = "one arm per subcommand")]
async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    // Tracing with the `suppaftp=info` security cap (issue #43, hard rule
    // 10): without this a `RUST_LOG=trace` would dump suppaftp's
    // `PASS <password>`.
    //
    // The guard IS HELD until the end of `run` (roadmap item 9): the
    // file's writer is non-blocking and its thread drains the queue when
    // dropped, so letting it drop here would throw away exactly the last
    // lines — the ones for the failure someone is diagnosing.
    // `[log]` comes from the config, so it is loaded BEFORE the
    // subscriber. It is a read of small files and without it the CLI and
    // the daemon would write somewhere different from where the frontends
    // write — with `norte doctor` pointing at one of the two, which is
    // worse than pointing at neither.
    let cfg_log = norte_config::load(&norte_config::standard_layers()).ok();
    let log_cfg = norte_core::logging::LogConfig {
        dir: cfg_log.as_ref().and_then(|c| c.log.dir.as_deref()),
        retain: cfg_log.as_ref().and_then(|c| c.log.retain),
        // The shared file: it is the one `norte doctor` reads.
        prefix: None,
        format: cfg_log.as_ref().map(|c| c.log.format).unwrap_or_default(),
    };
    // `norte daemon run` — and only it — also sets up an in-memory ring
    // (#328, ADR 0092): it is the registry `log.tail` serves to a
    // frontend living in another process, and without it the window
    // paints the wrong process's ring (#326). Everything else keeps plain
    // `init`, because it has nobody to show it to and would pay two
    // thousand lines of memory for nobody — and that includes `norte
    // daemon stop`, which is a client that sends one request and dies.
    //
    // `init_with_ring` and not `init_to_file_with_ring`: this path already
    // set up `init`, i.e. WITH stderr, so the other function would not
    // have added a ring but REMOVED a layer. Whoever starts `norte daemon
    // run` in a terminal to see why it won't come up would stop reading
    // anything.
    #[cfg(unix)]
    let log_ring = if matches!(
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

    // The daemon builds ITS OWN engine (with journal+policy, M3-4): the
    // embedded one below is only for the rest of the subcommands.
    #[cfg(unix)]
    if let Cmd::Daemon { cmd } = cli.cmd {
        return cmd::daemon::daemon_cmd(cmd, log_ring).await;
    }
    // MCP/policy/undo talk to the daemon directly as a client (they do not
    // go through the embedded Backend): the daemon owns the journal and
    // the policy.
    #[cfg(unix)]
    match cli.cmd {
        Cmd::Mcp { cmd } => return cmd::daemon::mcp_cmd(cmd, cli.socket).await,
        Cmd::Policy { cmd } => return cmd::daemon::policy_cmd(cmd, cli.socket).await,
        Cmd::Undo { session } => return cmd::daemon::undo_cmd(&session, cli.socket).await,
        Cmd::Ai { cmd } => return cmd::ai::ai_cmd(cmd).await,
        _ => {}
    }
    // Audit reads the journal DB directly (read-only, daemon stopped).
    if let Cmd::Audit { cmd } = cli.cmd {
        return cmd::audit::audit_cmd(cmd).await;
    }
    // A frontend is a SEPARATE process (the TUI takes over the terminal;
    // the graphical one, when there is one, will open a window): this CLI
    // only locates it and hands it the process — no engine or daemon
    // here.
    if let Cmd::Tui { ref args } = cli.cmd {
        return exec_frontend(TUI_BIN, args);
    }
    // Doctor is read-only over config/keymaps (H2): neither engine nor daemon.
    if let Cmd::Doctor { json } = cli.cmd {
        return cmd::entorno::doctor_cmd(json).await;
    }
    // `paths` resolves paths and stats them, nothing more: same place for
    // the same reason. And BEFORE building an engine: the question "where
    // is my config" is asked exactly when something about it is broken.
    if let Cmd::Paths { json } = cli.cmd {
        return cmd::entorno::paths_cmd(json, cli.socket.clone()).await;
    }
    // Help is the EMBEDDED corpus plus the user's keymap (H3g): no
    // engine, no daemon, no network. It goes up here for that reason —
    // building an engine to print documentation would be work the reader
    // pays for without seeing it.
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
    // `theme import` reads a JSON and writes a TOML into the config dir:
    // no engine or daemon, and all synchronous, so `spawn_blocking` (hard
    // rule 2).
    if let Cmd::Theme { ref cmd } = cli.cmd {
        let cmd = cmd.clone();
        return tokio::task::spawn_blocking(move || theme::run(&cmd)).await?;
    }

    // Search index (M4, ADR 0034): the SAME file the daemon uses
    // (config_dir/index.db), so an embedded `norte index build` persists
    // and a later query reads it. It is ONLY opened in embedded mode:
    // with `--daemon` the index's owner is the daemon (accessed via RPC),
    // and opening it here would only risk write contention. If it does
    // not open, it continues without it.
    let mut avisos = Vec::new();
    let engine = if cli.daemon {
        Engine::new()
    } else {
        // #167: hard rule 4 — whatever MUTATES the tree gets recorded. The
        // journal is the SAME file the daemon would open
        // (`config_dir()/journal.db`), and that equality is the point:
        // `norte undo` against the daemon has to see what an embedded
        // `norte mv` did. If another process holds the lock, it continues
        // without recording and with a warning — see `norte_core::embedded`.
        //
        // It is set up for ALL subcommands, including `norte ls`, and that
        // takes nobody's journal away: since #177 the file is not opened
        // until the first mutation. There used to be a hand-maintained
        // list of subcommands "that mutate" here — with `norte ai rename`
        // already outside it, opening its own journal on its own — and
        // that is the list this change made unnecessary.
        let dir = norte_core::connect::config_dir();
        let base = norte_core::embedded::engine_in(&dir);
        norte_core::equipo::con_indice(base, &dir, &mut avisos).await
    };
    if !cli.daemon {
        // Local provider, connector and — only for the two commands that
        // use them — the embeddings: what every engine carries
        // (`norte_core::equipo`). Loading `[ai]` resolves secrets, and an
        // `ls` never pays for that. With `--daemon` the daemon owns all
        // of this.
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
        // `[archive]` also in embedded mode: without this a `norte ls`
        // inside a zip used the default limits even if `norte.toml` set
        // others. Broken, here it is a warning; in the daemon, a startup
        // error.
        if let Err(e) = norte_core::archive_config::aplicar(&engine).await {
            avisos.push(norte_core::equipo::Aviso::ArchivoInvalido(e.to_string()));
        }
    }
    for aviso in &avisos {
        eprintln!("{}", cmd::daemon::warning_text(aviso));
    }

    // #177: if this session ends up mutating without a journal, it is
    // said to stderr, on the spot. No-op on `--daemon`'s engine (that one
    // cannot end up without a journal: the owner is the daemon, on the
    // other side of the socket).
    engine.set_journal_warning_sink(Arc::new(JournalWarningStderr));
    // `sync.plan`/`sync.apply` retain the approved plan in an on-disk
    // spool (ADR 0049); without installing it the embedded arm answers
    // `Unsupported` — see `Backend::is_journalled`'s rustdoc. It is only
    // installed for `norte sync`, with the same LAZY criterion as `[ai]`
    // above: the other subcommands do not need it, and no sweep is needed
    // here (unlike the daemon's startup) because each plan's TTL reaps
    // itself, in `Spool::open`/`Spool::create`.
    if !cli.daemon && matches!(cli.cmd, Cmd::Sync { .. }) {
        engine.set_spool(norte_core::sync::Spool::new(
            norte_core::connect::config_dir(),
        ));
    }
    let mut backend = cmd::daemon::make_backend(engine, cli.daemon, cli.socket.clone()).await?;
    // #44: takes the degradation-warning channel BEFORE running the
    // command (in embedded mode this INSTALLS the observer, which fires
    // synchronously within the setup; remotely it takes the daemon's pump
    // receiver). Drained to stderr after the command — "never silent"
    // (ADR 0015 F). Remotely it is best-effort: the pump is concurrent
    // and the warning also stays in the daemon's `tracing::warn!`.
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
            // An `Err` from these two commands must NOT exit via `main`'s
            // `ExitCode::FAILURE`: see `code_for_could_not`.
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
            .unwrap_or_else(|e| cmd::compare::code_for_could_not(&e)))
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
            // Same reason as in `Cmd::Compare`: here 1 means "applied", so
            // no error can share it.
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
            .unwrap_or_else(|e| cmd::compare::code_for_could_not(&e)))
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
                // First TOFU contact: confirm and retry ONCE.
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
        | Cmd::Tui { .. } => unreachable!("handled above"),
        #[cfg(unix)]
        Cmd::Daemon { .. } | Cmd::Mcp { .. } | Cmd::Policy { .. } | Cmd::Undo { .. } => {
            unreachable!("handled above")
        }
    };
    report_degradations(&mut degraded);
    result
}

/// Drains the accumulated TLS degradation warnings (#44) to stderr —
/// "never silent" (ADR 0015 F). Best-effort in daemon mode (the pump is
/// concurrent and the warning also stays in the daemon's
/// `tracing::warn!`).
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

    /// The CLI launches the frontend by BINARY NAME, so the name it passes
    /// has to be the one the manifest builds. A string that matches no
    /// binary compiles equally well and fails at `exec`, with the user
    /// watching: `norte tui` stops working and nothing says so beforehand.
    ///
    /// The expected name is READ from the sibling crate's `Cargo.toml`,
    /// not written here: a constant in the test would get renamed by the
    /// same find-and-replace that would break the code, and then the test
    /// would keep the defect company instead of catching it.
    #[test]
    fn tui_launches_the_binary_the_manifest_builds() {
        let manifest =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../norte-tui/Cargo.toml");
        let toml = std::fs::read_to_string(&manifest).expect("the TUI's manifest");
        let expected = toml
            .split("[[bin]]")
            .nth(1)
            .and_then(|s| s.lines().find_map(|l| l.trim().strip_prefix("name = ")))
            .map(|n| n.trim_matches('"').to_owned())
            .expect("the manifest declares [[bin]] name");
        assert_eq!(
            super::TUI_BIN,
            expected,
            "the CLI launches `{}` and the manifest builds `{expected}`",
            super::TUI_BIN
        );
    }

    /// The sibling next door WINS over `PATH`: `norte` and `norte-tui` are
    /// installed together, and mixing them with another batch is exactly
    /// the failure that cost a debugging session (a July binary reading
    /// an August config).
    #[test]
    fn prefers_the_sibling_binary_and_falls_back_to_path() {
        let d = tempfile::tempdir().expect("tempdir");
        let exe = d.path().join("norte");
        std::fs::write(&exe, b"#!/bin/true\n").expect("write");
        // No sibling yet: bare name so PATH resolves it.
        assert_eq!(
            frontend_program(Some(&exe), "norte-tui"),
            std::path::PathBuf::from("norte-tui")
        );
        // With a sibling: absolute path to THAT one.
        let sibling = d.path().join("norte-tui");
        std::fs::write(&sibling, b"#!/bin/true\n").expect("write");
        assert_eq!(frontend_program(Some(&exe), "norte-tui"), sibling);
        // Without knowing where we are: PATH decides.
        assert_eq!(
            frontend_program(None, "otro-frontend"),
            std::path::PathBuf::from("otro-frontend")
        );
    }
}
