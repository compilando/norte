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
use norte_core::backend::{Backend, TaskRef};
use norte_core::{Engine, TransferOptions};
use norte_proto::{Entry, EntryKind, SymlinkPolicy, TaskState, VPath};
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

mod doctor;
mod help;

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

/// Nombre del binario del frontend GRÁFICO. Mismo criterio que [`TUI_BIN`],
/// sin el cruce: `norte-gui` está fuera del `default-members`, así que su
/// manifiesto no se compila en el gate del core.
const GUI_BIN: &str = "norte-gui";

/// Localiza un binario HERMANO (`norte-tui`/`norte-gui`) y le cede el
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
    version,
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
    /// Abre el frontend GRÁFICO (`norte-gui`). Los argumentos viajan tal
    /// cual al binario
    #[command(disable_help_flag = true)]
    Gui {
        /// Argumentos para `norte-gui`, verbatim
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Diagnósticos de solo lectura sobre capas de config y keymaps (H2)
    Doctor {
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
    /// plan, y pregunta antes de aplicar
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
        eprintln!("aviso: {}", why.text());
    }
}

#[allow(clippy::too_many_lines)]
async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    // Tracing con el cap de seguridad `suppaftp=info` (issue #43, regla 10):
    // sin esto un `RUST_LOG=trace` volcaría `PASS <password>` de suppaftp.
    norte_core::logging::init();

    // El daemon construye SU PROPIO engine (con journal+policy, M3-4): el
    // embebido de abajo es solo para el resto de subcomandos.
    #[cfg(unix)]
    if let Cmd::Daemon { cmd } = cli.cmd {
        return daemon_cmd(cmd).await;
    }
    // MCP/policy/undo hablan al daemon directamente como cliente (no van por
    // el Backend embebido): el daemon es el dueño del journal y la policy.
    #[cfg(unix)]
    match cli.cmd {
        Cmd::Mcp { cmd } => return mcp_cmd(cmd, cli.socket).await,
        Cmd::Policy { cmd } => return policy_cmd(cmd, cli.socket).await,
        Cmd::Undo { session } => return undo_cmd(&session, cli.socket).await,
        Cmd::Ai { cmd } => return ai_cmd(cmd).await,
        _ => {}
    }
    // Audit lee la DB del journal directamente (solo-lectura, daemon parado).
    if let Cmd::Audit { cmd } = cli.cmd {
        return audit_cmd(cmd).await;
    }
    // Los frontends son procesos APARTE (el TUI toma la terminal, la GUI
    // abre ventana): este CLI solo los localiza y les cede el proceso —
    // nada de engine ni daemon aquí.
    match cli.cmd {
        Cmd::Tui { ref args } => return exec_frontend(TUI_BIN, args),
        Cmd::Gui { ref args } => return exec_frontend(GUI_BIN, args),
        _ => {}
    }
    // Doctor es solo-lectura sobre config/keymaps (H2): ni engine ni daemon.
    if let Cmd::Doctor { json } = cli.cmd {
        return doctor_cmd(json).await;
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
        return Ok(shell_init_cmd(shell));
    }

    // Índice de búsqueda (M4, ADR 0034): el MISMO fichero que el daemon
    // (config_dir/index.db), así `norte index build` en embebido persiste y una
    // query posterior lo lee. SOLO se abre en modo embebido: con `--daemon` el
    // dueño del índice es el daemon (se accede por RPC), y abrirlo aquí solo
    // arriesgaría contención de escritura. Si no abre, se sigue sin él.
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
        let base = norte_core::embedded::engine_in(&norte_core::connect::config_dir());
        let index_path = norte_core::connect::config_dir().join("index.db");
        match norte_core::Index::open(&index_path).await {
            Ok(idx) => base.with_index(Arc::new(idx)),
            Err(e) => {
                eprintln!("aviso: índice no disponible ({e}); index.* dará Unsupported");
                base
            }
        }
    };
    engine.register_provider(Arc::new(LocalProvider::os_root()) as Arc<dyn Provider>);
    // Conexiones remotas bajo demanda (fase 6e): connections.toml +
    // known_hosts + secretos en el dir de config del usuario.
    engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
        norte_core::connect::config_dir(),
    )));
    // Embeddings (M4-IA-2): el engine embebido lleva el índice (arriba) pero
    // sin proveedor `index embed`/`semantic` darían Unsupported aun con
    // `[ai]` + embed_provider configurados. El bloque es LAZY: cargar `[ai]`
    // (config + resolución de secreto) solo lo pagan los dos comandos que lo
    // consumen — jamás un `ls`. Con `--daemon` el dueño del proveedor es el
    // daemon (su propio wiring en daemon-run).
    if !cli.daemon
        && matches!(
            cli.cmd,
            Cmd::Index {
                cmd: IndexCmd::Embed { .. } | IndexCmd::Semantic { .. }
            }
        )
    {
        match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
            Ok(Ok(config)) => {
                if let Some(w) = norte_core::ai::install_embed_provider(&engine, &config).await {
                    eprintln!("{w}");
                }
                engine.set_ai_config(config);
            }
            Ok(Err(e)) => eprintln!("aviso: [ai] inválido ({e})"),
            Err(e) => eprintln!("aviso: carga de [ai] falló ({e})"),
        }
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
    let mut backend = make_backend(engine, cli.daemon, cli.socket).await?;
    // #44: toma el canal de avisos de degradación ANTES de correr el comando
    // (en embebido esto INSTALA el observer, que dispara síncrono dentro del
    // establecimiento; en remoto toma el receptor del pump del daemon). Se
    // drena a stderr tras el comando — "nunca silencioso" (ADR 0015 F). En
    // remoto es best-effort: el pump es concurrente y el aviso también queda
    // en el `tracing::warn!` del daemon.
    let mut degraded = backend.take_degraded();
    let result = match cli.cmd {
        Cmd::Ls { path, json, attrs } => ls(&backend, &path, json, &attrs).await,
        Cmd::Compare {
            a,
            b,
            json,
            criteria,
            max_depth,
            mtime_tolerance_ms,
        } => {
            compare_cmd(
                &backend,
                &a,
                &b,
                json,
                &criteria,
                max_depth,
                mtime_tolerance_ms,
            )
            .await
        }
        Cmd::Sync {
            source,
            dest,
            mode,
            dry_run,
            // `--yes` no tiene nada que gobernar todavía: la pregunta y el
            // apply son la tarea 3, en su propio commit revisable.
            yes: _yes,
            criteria,
            mtime_tolerance_ms,
        } => {
            sync_cmd(
                &backend,
                &source,
                &dest,
                mode,
                dry_run,
                &criteria,
                mtime_tolerance_ms,
            )
            .await
        }
        Cmd::Connect { target } => connect_cmd(&backend, &target, cli.daemon).await,
        Cmd::Cp {
            src,
            dst,
            symlinks,
            resume,
        } => {
            let (from, to) = (vpath(&src)?, vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                resume: resume_policy(resume),
                ..TransferOptions::default()
            };
            let task = match backend.copy(&from, &to, opts).await {
                // Primer contacto TOFU: confirmar y reintentar UNA vez.
                Err(e) if tofu_confirm(&backend, &e).await? => backend.copy(&from, &to, opts).await,
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-copy"))?;
            Ok(run_task(task, true).await)
        }
        Cmd::Mv {
            src,
            dst,
            symlinks,
            resume,
        } => {
            let (from, to) = (vpath(&src)?, vpath(&dst)?);
            let opts = TransferOptions {
                symlinks: symlinks.into(),
                resume: resume_policy(resume),
                ..TransferOptions::default()
            };
            let task = match backend.move_(&from, &to, opts).await {
                Err(e) if tofu_confirm(&backend, &e).await? => {
                    backend.move_(&from, &to, opts).await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-move"))?;
            Ok(run_task(task, false).await)
        }
        Cmd::Gc {
            path,
            older_than_hours,
        } => gc_cmd(&backend, cli.daemon, &path, older_than_hours).await,
        Cmd::Rm { path } => {
            let target = vpath(&path)?;
            let task = match backend
                .delete(&target, norte_proto::DeleteMode::Permanent)
                .await
            {
                Err(e) if tofu_confirm(&backend, &e).await? => {
                    backend
                        .delete(&target, norte_proto::DeleteMode::Permanent)
                        .await
                }
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-delete"))?;
            Ok(run_task(task, false).await)
        }
        Cmd::Mkdir { path } => {
            let target = vpath(&path)?;
            let task = match backend.mkdir(&target).await {
                Err(e) if tofu_confirm(&backend, &e).await? => backend.mkdir(&target).await,
                other => other,
            }
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-enqueue-mkdir"))?;
            Ok(run_task(task, false).await)
        }
        Cmd::Plugin { cmd } => plugin_cmd(&backend, cmd).await,
        Cmd::Index { cmd } => index_cmd(&backend, cmd).await,
        Cmd::Audit { .. }
        | Cmd::Ai { .. }
        | Cmd::Doctor { .. }
        | Cmd::Help { .. }
        | Cmd::ShellInit { .. }
        | Cmd::Tui { .. }
        | Cmd::Gui { .. } => unreachable!("manejado arriba"),
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

/// `norte audit <verify|export|anchor>` (M3-5, ADR 0025): opera sobre la DB
/// del journal en SOLO-LECTURA. Con el daemon corriendo, `SQLite` devuelve
/// `database is locked` (su lock es exclusivo): el mensaje lo dice claro.
///
/// Desde #167 el daemon no es el único que puede tenerlo: un frontend embebido
/// —un `ntc` sin `--daemon`— se queda el mismo lock. Pero solo DESDE QUE MUTA
/// algo (#177): un `ntc` navegando no estorba a este comando, y por eso el
/// texto de ayuda no manda cerrar los frontends, solo dice quién puede tenerlo.
async fn audit_cmd(cmd: AuditCmd) -> anyhow::Result<ExitCode> {
    use norte_core::{Journal, audit};
    let dir = norte_core::connect::config_dir();
    let journal_path = dir.join("journal.db");
    let anchors_path = dir.join("journal-anchors.jsonl");
    let journal = Journal::open_read_only(&journal_path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-audit-open-failed"))?;
    match cmd {
        AuditCmd::Export { format } => {
            let entries = journal
                .entries()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context(norte_i18n::t("cli-audit-open-failed"))?;
            let out = match format {
                AuditFormat::Jsonl => audit::export_jsonl(&entries),
                AuditFormat::Csv => audit::export_csv(&entries),
            };
            print!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        AuditCmd::Anchor => {
            // Jamás se ancla una cadena que este binario no haya podido
            // verificar: el ancla fijaría como «buena» una historia rota, o una
            // que no sabe leer (ADR 0046).
            let status = journal
                .verify_chain()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if !status.is_intact() {
                let declared = journal.format().await.map_err(|e| anyhow::anyhow!("{e}"))?;
                report_chain_not_certified(&status, declared);
                return Ok(ExitCode::FAILURE);
            }
            let Some((seq, head)) = journal.head().await.map_err(|e| anyhow::anyhow!("{e}"))?
            else {
                println!("{}", norte_i18n::t("cli-audit-empty"));
                return Ok(ExitCode::SUCCESS);
            };
            // La clave del keyring puede bloquear (D-Bus/prompt): fuera del
            // reactor (regla 2).
            let key = tokio::task::spawn_blocking(norte_core::connect::journal_anchor_key)
                .await
                .map_err(|_| anyhow::anyhow!("keyring task panicked"))?
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            let line = audit::anchor_line(&key, &audit::Anchor { seq, head });
            append_line_0600(&anchors_path, &line).await?;
            // La línea sale por stdout A PROPÓSITO: la copia EXTERNA de las
            // anclas (log remoto, otro host) es lo que hace detectable el
            // recorte del fichero local (ADR 0025).
            println!("{line}");
            println!(
                "{}",
                norte_i18n::ta("cli-audit-anchored", &[("seq", &seq.to_string())])
            );
            Ok(ExitCode::SUCCESS)
        }
        AuditCmd::Verify { allow_no_anchors } => {
            audit_verify(&journal, &anchors_path, allow_no_anchors).await
        }
    }
}

/// Por qué la cadena NO quedó certificada, en la voz que corresponde: una
/// rotura es una acusación y se cita dónde; un formato desconocido (ADR 0046)
/// NO lo es —este binario no sabe recomputar lo que escribió uno más nuevo— y
/// se dice sin acusar a nadie, pero también sin absolver: en los dos casos el
/// audit sale con FALLO.
///
/// Va por STDOUT, igual que `cli-audit-chain-ok`: el veredicto es la SALIDA del
/// audit, no un diagnóstico suelto, y un `norte audit verify > informe.txt` que
/// guarde la cobertura y las anclas pero no el veredicto es justo el fichero
/// que no hay que producir. El fallo lo lleva el código de salida.
///
/// `declared` viene del journal porque el veredicto `Broken` no lo lleva: una
/// cadena rota EN un journal que además está escrito en un formato ilegible es
/// una rotura que hay que leer con esa luz.
fn report_chain_not_certified(
    status: &norte_core::ChainStatus,
    declared: norte_core::JournalFormat,
) {
    use norte_core::{ChainStatus, JournalFormat};
    // `Unmarked` no llega por la rama de formato desconocido (un journal sin
    // marcador se verifica con las reglas de hoy), y cualquier variante futura
    // es, por definición, algo que este binario no sabe leer.
    let name = |f: JournalFormat| match f {
        JournalFormat::Version(v) => v.to_string(),
        _ => norte_i18n::t("cli-audit-format-unreadable"),
    };
    let unknown_format = |declared: JournalFormat, known: u32| {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-chain-unknown-format",
                &[("declared", &name(declared)), ("known", &known.to_string())],
            )
        );
    };
    match status {
        ChainStatus::Broken { first_bad_seq } => {
            if declared.is_unknown() {
                unknown_format(declared, norte_core::JOURNAL_FORMAT);
            }
            println!(
                "{}",
                norte_i18n::ta(
                    "cli-audit-chain-broken",
                    &[("seq", &first_bad_seq.to_string())],
                )
            );
        }
        ChainStatus::UnknownFormat {
            declared,
            known,
            first_unverifiable_seq,
        } => {
            unknown_format(*declared, *known);
            if let Some(seq) = first_unverifiable_seq {
                println!(
                    "{}",
                    norte_i18n::ta(
                        "cli-audit-chain-unverifiable-from",
                        &[("seq", &seq.to_string())],
                    )
                );
            }
        }
        // Un veredicto que este binario no conoce se trata como NO certificado.
        // `ChainStatus` es `#[non_exhaustive]` justamente para que un veredicto
        // nuevo llegue aquí en vez de colarse por la rama de «íntegra».
        _ => println!("{}", norte_i18n::t("cli-audit-chain-not-certified")),
    }
}

/// `norte audit verify`: cadena (cita la primera rotura, B2) + anclas +
/// COBERTURA (hasta qué seq llegan las anclas Ok — el recorte del fichero de
/// anclas se manifiesta como cobertura que retrocede). Sin anclas = FALLO
/// salvo `--allow-no-anchors`: la ausencia es indistinguible de un borrado
/// hostil (H1 del security-reviewer).
async fn audit_verify(
    journal: &norte_core::Journal,
    anchors_path: &std::path::Path,
    allow_no_anchors: bool,
) -> anyhow::Result<ExitCode> {
    use norte_core::{ChainStatus, audit};
    let status = journal
        .verify_chain()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Una cadena ROTA ya trae su culpable y su sitio: no hay segunda opinión
    // que buscar. Una que este binario NO SABE LEER (ADR 0046) es al revés —
    // las anclas son la única evidencia que discrimina «journal más nuevo» de
    // «marcador re-declarado», no necesitan recomputar la cadena (contrastan
    // hashes ALMACENADOS) y el operador ya las tiene en disco. Así que se sigue
    // hasta el informe de anclas y se sale con FALLO igual.
    let certified = match status {
        ChainStatus::Intact { entries } => {
            println!(
                "{}",
                norte_i18n::ta("cli-audit-chain-ok", &[("entries", &entries.to_string())])
            );
            true
        }
        ChainStatus::UnknownFormat { .. } => {
            report_chain_not_certified(&status, declared_format(journal).await?);
            false
        }
        _ => {
            report_chain_not_certified(&status, declared_format(journal).await?);
            return Ok(ExitCode::FAILURE);
        }
    };
    let head_seq = journal
        .head()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|(seq, _)| seq);
    let lines = match tokio::fs::read_to_string(&anchors_path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let msg = norte_i18n::t("cli-audit-no-anchors");
            if allow_no_anchors {
                println!("{msg}");
                // `--allow-no-anchors` perdona la AUSENCIA de anclas, no una
                // cadena sin certificar.
                return Ok(exit_for(certified));
            }
            // Ausencia = fallo por defecto: un atacante sin clave puede
            // BORRAR el fichero; solo el humano decide que «no hay» es ok.
            eprintln!("{msg}");
            return Ok(ExitCode::FAILURE);
        }
        Err(e) => return Err(e).context("journal-anchors.jsonl"),
    };
    let key = tokio::task::spawn_blocking(norte_core::connect::journal_anchor_key)
        .await
        .map_err(|_| anyhow::anyhow!("keyring task panicked"))?
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // UN snapshot de la cadena para todo el veredicto (sin TOCTOU entre el
    // verify de arriba y los contrastes de anclas).
    let hash_by_seq: std::collections::HashMap<i64, [u8; 32]> = journal
        .entries()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_iter()
        .filter_map(|e| e.entry_hash.try_into().ok().map(|h: [u8; 32]| (e.seq, h)))
        .collect();
    let report = audit::verify_anchors(&key, &lines, &hash_by_seq);
    report_anchors(&report, head_seq, certified);
    if !report.bad.is_empty() {
        return Ok(ExitCode::FAILURE);
    }
    if certified {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchors-ok",
                &[("count", &report.checked.to_string())],
            )
        );
    }
    Ok(exit_for(certified))
}

/// El informe de anclas: las malas una por una, la salvedad cuando la cadena
/// NO quedó certificada, y la cobertura.
///
/// El orden importa. «Ancladas contra la cadena» presupone una cadena
/// verificada; si este binario no pudo verificarla, lo que las anclas dicen es
/// OTRA frase —los hashes almacenados no se han movido desde que se ancló— y
/// esa salvedad va ANTES de la cobertura, porque «hasta el seq 100 de 100»
/// leída sin ella es la línea que el operador citará como visto bueno.
fn report_anchors(
    report: &norte_core::audit::AnchorsReport,
    head_seq: Option<i64>,
    certified: bool,
) {
    for (line_no, verdict) in &report.bad {
        let Some(detail) = verdict_detail(verdict) else {
            continue;
        };
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchor-bad",
                &[("line", &line_no.to_string()), ("detail", &detail)],
            )
        );
    }
    if !certified {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchors-ok-unverified-chain",
                &[("count", &report.checked.to_string())],
            )
        );
    }
    // Cobertura SIEMPRE visible: anclas hasta X, cadena hasta Y. Un recorte
    // del fichero de anclas retrocede X sin tocar la cadena.
    println!(
        "{}",
        norte_i18n::ta(
            "cli-audit-coverage",
            &[
                (
                    "anchored",
                    &report
                        .max_ok_seq
                        .map_or_else(|| "-".into(), |s| s.to_string()),
                ),
                (
                    "head",
                    &head_seq.map_or_else(|| "-".into(), |s| s.to_string()),
                ),
            ],
        )
    );
}

/// El formato que DECLARA el journal, para poner el veredicto en contexto.
async fn declared_format(
    journal: &norte_core::Journal,
) -> anyhow::Result<norte_core::JournalFormat> {
    journal.format().await.map_err(|e| anyhow::anyhow!("{e}"))
}

/// Éxito solo si la cadena quedó certificada.
fn exit_for(certified: bool) -> ExitCode {
    if certified {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Traduce un veredicto NO-Ok de ancla a su mensaje Fluent.
fn verdict_detail(verdict: &norte_core::audit::AnchorVerdict) -> Option<String> {
    use norte_core::audit::AnchorVerdict;
    Some(match verdict {
        AnchorVerdict::BadLine => norte_i18n::t("cli-audit-verdict-bad-line"),
        AnchorVerdict::BadMac => norte_i18n::t("cli-audit-verdict-bad-mac"),
        AnchorVerdict::MissingSeq(a) => {
            norte_i18n::ta("cli-audit-verdict-missing", &[("seq", &a.seq.to_string())])
        }
        AnchorVerdict::HashMismatch(a) => {
            norte_i18n::ta("cli-audit-verdict-mismatch", &[("seq", &a.seq.to_string())])
        }
        AnchorVerdict::Ok(_) => return None,
    })
}

/// Text-mode line for one [`doctor::Finding`] (review MINOR-3): `Finding`
/// itself carries only STABLE, MACHINE detail (ids, paths, var names — see
/// `doctor.rs`'s own module doc) so `--json` stays locale-free. A few codes
/// had a full narrative sentence baked into `detail` as English prose before
/// this review (`plugin-digest-stale`, `connections-none`); that sentence now
/// lives HERE, keyed by `code`, and is looked up ONLY for text-mode
/// rendering — every other code still prints its raw (already-machine)
/// `detail` unchanged.
fn doctor_finding_line(f: &doctor::Finding) -> String {
    match f.code {
        "connections-parse" => norte_i18n::t("cli-doctor-detail-connections-parse"),
        "connections-none" => norte_i18n::t("cli-doctor-detail-connections-none"),
        "plugin-digest-stale" => norte_i18n::ta(
            "cli-doctor-detail-plugin-digest-stale",
            &[("id", &f.detail)],
        ),
        // H3e `plugin-help-*`: same policy — `detail` stays the machine value
        // (the plugin id; for `foreign-command`, `{id}: {command}`, already
        // masked and capped at the `doctor` boundary) and the sentence lives
        // here.
        "plugin-help-truncated" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-truncated",
            &[("id", &f.detail)],
        ),
        "plugin-help-lossy" => {
            norte_i18n::ta("cli-doctor-detail-plugin-help-lossy", &[("id", &f.detail)])
        }
        "plugin-help-empty" => {
            norte_i18n::ta("cli-doctor-detail-plugin-help-empty", &[("id", &f.detail)])
        }
        "plugin-help-bad-header" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-bad-header",
            &[("id", &f.detail)],
        ),
        "plugin-help-foreign-command" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-foreign-command",
            &[("detail", &f.detail)],
        ),
        "plugin-help-shadows-topic" => norte_i18n::ta(
            "cli-doctor-detail-plugin-help-shadows-topic",
            &[("id", &f.detail)],
        ),
        _ => f.detail.clone(),
    }
}

/// `norte shell-init <SHELL>` (S3, shell-integration): prints the wrapper's
/// source for the caller's rc file to `eval` (bash/zsh) or `source` (fish).
/// Pure lookup — [`norte_frontend::shell::Shell`] owns the actual text — so
/// this is early-returned in `run()` like `doctor_cmd`/`help::run`, ahead of
/// any engine/daemon wiring.
fn shell_init_cmd(shell: &str) -> ExitCode {
    if let Some(sh) = norte_frontend::shell::Shell::parse(shell) {
        // No trailing newline of our own: the wrapper's own text already
        // ends in one, and `eval "$(norte shell-init bash)"` runs command
        // substitution either way (it strips trailing newlines itself).
        print!("{}", sh.wrapper());
        ExitCode::SUCCESS
    } else {
        eprintln!(
            "{}",
            norte_i18n::ta("cli-shell-init-unknown", &[("shell", shell)])
        );
        ExitCode::FAILURE
    }
}

/// `norte doctor` (H2): read-only diagnostics over config layers, keymaps,
/// plugins and connections. Never touches the daemon/engine — early-returned
/// in `run()` like `audit_cmd`.
async fn doctor_cmd(json: bool) -> anyhow::Result<ExitCode> {
    let layers = norte_config::standard_layers();
    // Plugins/connections live under the SINGLE resolved config dir (ADR
    // 0035's `norte_config::config_dir` — the same one `norte_core::connect`
    // re-exports and every other command in this binary already uses for
    // `connections.toml`/`journal.db`/`policy.toml`), NOT `layers.dirs.last()`
    // — that is the PROJECT layer (`.norte`, lowest precedence but last in
    // the ascending-precedence `Layers::dirs` list), a different directory.
    let config_dir = norte_config::config_dir();
    // `doctor::check_*` do synchronous fs I/O (norte-config's own design —
    // see its crate doc); never call them directly on the async executor
    // (rule 2).
    let findings = tokio::task::spawn_blocking(move || {
        let env = |k: &str| std::env::var_os(k);
        let mut findings = doctor::check_config(&layers, &env);
        findings.extend(doctor::check_columns(&layers));
        findings.extend(doctor::check_keymaps(&layers));
        findings.extend(doctor::check_plugins(&config_dir));
        findings.extend(doctor::check_connections(&config_dir, &env));
        findings
    })
    .await
    .map_err(|e| anyhow::anyhow!("doctor: {e}"))?;

    if json {
        #[derive(serde::Serialize)]
        struct Summary {
            errors: usize,
            warnings: usize,
        }
        #[derive(serde::Serialize)]
        struct Report<'a> {
            findings: &'a [doctor::Finding],
            summary: Summary,
        }
        let errors = findings
            .iter()
            .filter(|f| f.severity == doctor::Severity::Error)
            .count();
        let warnings = findings
            .iter()
            .filter(|f| f.severity == doctor::Severity::Warn)
            .count();
        serde_json::to_writer_pretty(
            std::io::stdout().lock(),
            &Report {
                findings: &findings,
                summary: Summary { errors, warnings },
            },
        )
        .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        println!("{}", norte_i18n::t("cli-doctor-title"));
        let mut last_section = "";
        for f in &findings {
            if f.section != last_section {
                let key = match f.section {
                    "config" => "cli-doctor-section-config",
                    "keymap" => "cli-doctor-section-keymap",
                    "plugins" => "cli-doctor-section-plugins",
                    "connections" => "cli-doctor-section-connections",
                    // No debería ocurrir (`Finding::section` es un catálogo
                    // cerrado en este módulo): se imprime crudo en vez de
                    // panicar — un diagnóstico jamás debe tumbar el proceso.
                    other => other,
                };
                println!("{}", norte_i18n::t(key));
                last_section = f.section;
            }
            let marker = match f.severity {
                doctor::Severity::Ok => norte_i18n::t("cli-doctor-ok"),
                doctor::Severity::Warn => norte_i18n::t("cli-doctor-warn"),
                doctor::Severity::Error => norte_i18n::t("cli-doctor-error"),
            };
            println!("  [{marker}] {}: {}", f.code, doctor_finding_line(f));
        }
        println!("{}", norte_i18n::t("cli-doctor-footer-keymap-approx"));
        println!(
            "{}",
            norte_i18n::t("cli-doctor-footer-connections-not-probed")
        );
        // S3 (shell-integration): unconditional, like the two footers above
        // — whether the wrapper is actually `eval`ed in the caller's rc file
        // cannot be observed from this process, so the honest answer is the
        // instruction, not a Finding with a severity that would claim more
        // than can be known.
        println!("{}", norte_i18n::t("cli-doctor-footer-shell-init"));
    }
    let has_error = findings
        .iter()
        .any(|f| f.severity == doctor::Severity::Error);
    Ok(if has_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

/// Appendea una línea (+`\n`) a `path`, creándolo `0600` si no existe.
async fn append_line_0600(path: &std::path::Path, line: &str) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;
    let mut opts = tokio::fs::OpenOptions::new();
    opts.append(true).create(true);
    #[cfg(unix)]
    opts.mode(0o600);
    let mut f = opts.open(path).await.context("journal-anchors.jsonl")?;
    f.write_all(line.as_bytes()).await?;
    f.write_all(b"\n").await?;
    f.flush().await?;
    Ok(())
}

/// `norte plugin run <id> <command> [arg]`: ejecuta un comando de un plugin YA
/// aprobado+activado por el humano y escribe su salida a STDOUT. Va por el
/// `Backend` elegido con los flags globales (`--daemon`/`--socket`), como el
/// resto de operaciones (regla 7).
async fn plugin_cmd(backend: &Backend, cmd: PluginCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        PluginCmd::Run { id, command, arg } => {
            match backend.plugin_run_command(&id, &command, &arg).await {
                Ok(output) => {
                    println!("{output}");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!(
                        "{}",
                        norte_i18n::ta("cli-plugin-run-failed", &[("error", &e.to_string())])
                    );
                    Ok(ExitCode::FAILURE)
                }
            }
        }
        // Copia local, SIN daemon: instalar es mover ficheros al directorio de
        // config, y hacerlo depender de un daemon vivo sería pedirle al usuario
        // que arranque el programa para poder instalarle algo.
        PluginCmd::Install { path, force } => {
            let dir = norte_core::connect::config_dir();
            match norte_core::plugins::install(&dir, &path, force) {
                Ok(rep) => {
                    let verbo = if rep.replaced {
                        "reemplazado"
                    } else {
                        "instalado"
                    };
                    // El nombre viene del manifiesto de un tercero: se pinta
                    // saneado, como en el gestor.
                    let (nombre, _) = norte_frontend::display_name(rep.name.as_bytes());
                    println!("{verbo}: {} ({nombre})", rep.id);
                    if rep.replaced {
                        println!(
                            "consentimiento RETIRADO: el `.wasm` es otro y el digest del \
                             manifiesto no lo habría notado"
                        );
                    }
                    println!("queda SIN aprobar: apruébalo y actívalo en el gestor de extensiones");
                    Ok(ExitCode::SUCCESS)
                }
                Err(e) => {
                    eprintln!("{e}");
                    Ok(ExitCode::FAILURE)
                }
            }
        }
    }
}

/// `norte index build|query` (M4, ADR 0034).
async fn index_cmd(backend: &Backend, cmd: IndexCmd) -> anyhow::Result<ExitCode> {
    match cmd {
        IndexCmd::Build { path } => {
            let root = vpath(&path)?;
            let task = backend
                .index_build(&root)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("no se pudo lanzar el index build")?;
            Ok(run_task(task, false).await)
        }
        IndexCmd::Query { path, text, limit } => {
            let root = vpath(&path)?;
            let hits = backend
                .index_query(&root, &text, limit)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("query del índice")?;
            for h in &hits {
                let marker = match h.kind {
                    EntryKind::Dir => "d",
                    EntryKind::Symlink => "l",
                    EntryKind::Other => "?",
                    EntryKind::File => "-",
                };
                let size = h.size.map_or_else(|| "-".to_string(), |s| s.to_string());
                // `display_lossy` sanea los bytes hostiles (regla 1): jamás
                // controles/no-UTF8 crudos por stdout.
                println!("{marker}\t{size}\t{}", h.path.display_lossy());
            }
            if hits.is_empty() {
                eprintln!("(sin resultados)");
            }
            Ok(ExitCode::SUCCESS)
        }
        IndexCmd::Embed { path } => {
            let root = vpath(&path)?;
            let task = backend
                .index_embed(&root)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("no se pudo lanzar el index embed")?;
            Ok(run_task(task, false).await)
        }
        IndexCmd::Semantic { text, root, k } => {
            let root = root.as_deref().map(vpath).transpose()?;
            let hits = backend
                .index_search_semantic(root.as_ref(), &text, k)
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context("búsqueda semántica")?;
            for h in &hits {
                // Paths con nombres arbitrarios hacia un terminal: MISMO
                // enmascarado marcado que el plan de `norte ai rename`
                // (`display_name` por segmento vía `path_display`, hazards
                // → � y el `!` delata la alteración).
                let (texto, hostil) = norte_frontend::path_display(&h.path);
                println!("{:.2}\t{}{texto}", h.score, if hostil { "!" } else { "" });
            }
            if hits.is_empty() {
                eprintln!("(sin resultados)");
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Resuelve el socket del daemon y un `spawn_cmd` de autoarranque (este mismo
/// binario sabe ser daemon). Sondas de FS fuera del runtime (regla 2).
#[cfg(unix)]
async fn socket_and_spawn(
    socket: Option<PathBuf>,
) -> anyhow::Result<(PathBuf, Vec<std::ffi::OsString>)> {
    let (socket, exe) = tokio::task::spawn_blocking(move || {
        let socket = socket.unwrap_or_else(|| norte_core::daemon::default_socket_path(None));
        (socket, std::env::current_exe())
    })
    .await
    .context("resolución del socket/exe")?;
    let exe = exe.context("current_exe")?;
    let spawn_cmd: Vec<std::ffi::OsString> = vec![
        exe.into(),
        "daemon".into(),
        "run".into(),
        "--socket".into(),
        socket.clone().into(),
    ];
    Ok((socket, spawn_cmd))
}

/// `norte mcp serve`: sirve MCP por stdio, arrancando el daemon si hace falta.
/// El puente conecta como sesión de agente; el tracing va a stderr (el
/// `logging::init` global ya lo fija), stdout es EXCLUSIVO del transporte MCP.
#[cfg(unix)]
async fn mcp_cmd(cmd: McpCmd, socket: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    let McpCmd::Serve { session } = cmd;
    let (socket, spawn_cmd) = socket_and_spawn(socket).await?;
    // Autoarranque idempotente: connect_or_spawn arranca el daemon si el
    // socket no responde, luego el puente reconecta.
    if let Err(e) = norte_core::daemon::Client::connect_or_spawn(&socket, move || {
        let mut cmd = std::process::Command::new(&spawn_cmd[0]);
        cmd.args(&spawn_cmd[1..]);
        cmd
    })
    .await
    {
        anyhow::bail!("no se pudo arrancar/alcanzar el daemon: {e}");
    }
    eprintln!(
        "{}",
        norte_i18n::ta("cli-mcp-serving", &[("session", &session)])
    );
    norte_mcp::bridge::serve_stdio(&socket, &session)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("el puente MCP terminó con error")?;
    Ok(ExitCode::SUCCESS)
}

/// `norte policy grant <request_id>`: un humano concede un scope pedido por un
/// agente. Conexión User (sin `agent_session`).
#[cfg(unix)]
async fn policy_cmd(cmd: PolicyCmd, socket: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    use norte_core::daemon::Client;
    let PolicyCmd::Grant { request_id } = cmd;
    let (socket, _spawn) = socket_and_spawn(socket).await?;
    let mut client = Client::connect(&socket)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no hay daemon en marcha")?;
    client
        .initialize(norte_proto::methods::ClientInfo {
            name: "norte-cli".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        })
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let _: norte_proto::methods::GrantScopeResult = client
        .call(
            norte_proto::methods::POLICY_GRANT_SCOPE,
            &norte_proto::methods::GrantScopeParams { request_id },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo conceder el scope")?;
    println!("{}", norte_i18n::t("cli-scope-granted"));
    Ok(ExitCode::SUCCESS)
}

/// `norte undo <session>`: un humano deshace la sesión completa de un agente.
/// Corre como Task; se espera su terminal con Ctrl-C = cancelar (patrón `cp`).
#[cfg(unix)]
async fn undo_cmd(session: &str, socket: Option<PathBuf>) -> anyhow::Result<ExitCode> {
    use norte_core::backend::remote::RemoteBackend;
    let (socket, spawn_cmd) = socket_and_spawn(socket).await?;
    let backend = Backend::Remote(
        RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                name: "norte-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?,
    );
    let task = backend
        .undo_session(session)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::ta("cli-undo-failed", &[("error", "submit")]))?;
    let task_id = task.id();
    let outcome = run_task(task, true).await;
    if outcome != ExitCode::SUCCESS {
        return Ok(outcome);
    }
    // «Done» CUALIFICADO (#71): el estado terminal de la Task es Completed
    // incluso si el LIFO se bloqueó o todo se saltó — el informe es la ÚNICA
    // señal de integridad. Solo un daemon N-1 (sin el método → Unsupported)
    // degrada al mensaje simple; cualquier otro fallo del fetch NO se traga:
    // aviso + exit≠0 (el undo pudo funcionar, pero queda sin verificar).
    let report = match backend.undo_report(task_id).await {
        Ok(r) => r,
        Err(norte_proto::Error::Unsupported) => {
            println!(
                "{}",
                norte_i18n::ta("cli-undo-done", &[("session", session)])
            );
            return Ok(outcome);
        }
        Err(e) => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-undo-report-unavailable", &[("error", &e.to_string())])
            );
            return Ok(ExitCode::FAILURE);
        }
    };
    println!(
        "{}",
        norte_i18n::ta(
            "cli-undo-report",
            &[
                ("undone", &report.undone.to_string()),
                (
                    "skipped_irreversible",
                    &report.skipped_irreversible.to_string(),
                ),
            ],
        )
    );
    if report.skipped_created_no_trash > 0 {
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-undo-left-in-place",
                &[("count", &report.skipped_created_no_trash.to_string())],
            )
        );
    }
    if let Some(blocked) = report.blocked {
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-undo-blocked",
                &[
                    ("seq", &blocked.seq.to_string()),
                    ("error", &blocked.error.to_string()),
                ],
            )
        );
        return Ok(ExitCode::FAILURE);
    }
    println!(
        "{}",
        norte_i18n::ta("cli-undo-done", &[("session", session)])
    );
    Ok(outcome)
}

/// Elige el transporte (regla 7: la lógica es la misma). `--daemon`
/// conecta al socket, arrancando `norte daemon run` si hace falta.
async fn make_backend(
    engine: Engine,
    daemon: bool,
    socket: Option<PathBuf>,
) -> anyhow::Result<Backend> {
    if !daemon {
        // #44: los avisos de degradación se drenan en el dispatch (top-level)
        // vía `Backend::take_degraded`, que en embebido instala el observer del
        // canal — misma vía que en modo daemon (rust M1 + security m1).
        return Ok(Backend::Embedded(Arc::new(engine)));
    }
    #[cfg(not(unix))]
    {
        let _ = socket;
        anyhow::bail!("--daemon no está disponible en Windows todavía (issue #33)");
    }
    #[cfg(unix)]
    {
        use norte_core::backend::remote::RemoteBackend;
        // Ambas sondas de FS (socket por defecto + current_exe) fuera del
        // runtime (regla 2, m3 del rust-reviewer).
        let (socket, exe) = tokio::task::spawn_blocking(move || {
            let socket = socket.unwrap_or_else(|| norte_core::daemon::default_socket_path(None));
            (socket, std::env::current_exe())
        })
        .await
        .context("resolución del socket/exe")?;
        let exe = exe.context("current_exe")?;
        // Autoarranque: este MISMO binario sabe ser daemon.
        let mut spawn_cmd: Vec<std::ffi::OsString> =
            vec![exe.into(), "daemon".into(), "run".into()];
        spawn_cmd.push("--socket".into());
        spawn_cmd.push(socket.clone().into());
        let remote = RemoteBackend::connect(
            socket,
            Some(spawn_cmd),
            norte_proto::methods::ClientInfo {
                name: "norte-cli".into(),
                version: env!("CARGO_PKG_VERSION").into(),
            },
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context("no se pudo hablar con el daemon")?;
        Ok(Backend::Remote(remote))
    }
}

/// `norte daemon run|stop` (ADR 0011). El engine que sirve el daemon es el
/// MISMO embebido de esta CLI: solo cambia el transporte (regla 7).
#[cfg(unix)]
#[allow(clippy::too_many_lines)]
async fn daemon_cmd(cmd: DaemonCmd) -> anyhow::Result<ExitCode> {
    use norte_core::daemon::{
        Client, Daemon, DaemonApprovalResolver, DaemonConfig, default_socket_path,
    };
    match cmd {
        DaemonCmd::Run {
            socket,
            idle_timeout,
        } => {
            // El daemon es el dueño ÚNICO del journal (spec §4, ADR 0024) y
            // quien instala policy + approvals: los agentes MCP se gobiernan
            // aquí, jamás en el puente.
            let journal_path = norte_core::connect::config_dir().join("journal.db");
            // El aviso NOMBRA la causa probable: desde #167 un frontend embebido
            // (un `ntc` sin `--daemon`) se queda el lock exclusivo, y sin esta
            // frase el operador recibe un texto de sqlx y ninguna pista de qué
            // cerrar. Desde #177 ese frontend solo lo tiene si YA MUTÓ algo, así
            // que el caso corriente —arrancar el daemon con un `ntc` abierto—
            // volvió a funcionar.
            let journal = norte_core::SqliteJournal::open(&journal_path)
                .await
                .context(
                    "no se pudo abrir el journal (si dice «database is locked», otro \
                     proceso norte lo tiene: ¿un `ntc` embebido, u otro daemon?)",
                )?;
            // Barrido de spools de sincronización (ADR 0049), junto al journal
            // y por lo mismo: un cierre violento deja detrás ficheros que
            // AUTORIZAN escrituras, y nadie más los va a recoger. Se llevan
            // todos, no solo los caducados — aquí no hay ninguna conexión viva
            // todavía, así que todo spool que exista es de una conexión muerta.
            //
            // Va DESPUÉS del journal a propósito: su lock exclusivo es lo que
            // garantiza que no hay otro daemon sobre este directorio de estado
            // al que le estemos barriendo un plan vivo.
            //
            // Un barrido que falla NO impide arrancar. Lo que impide aplicar un
            // plan de un arranque anterior no es esto, es que el registro de
            // planes emitidos vive en memoria y nace vacío; lo que queda en
            // disco es basura, y un daemon que se niega a arrancar por un
            // fichero que no se deja borrar es peor fallo que el que evita.
            //
            // El `Spool` se construye UNA vez y se clona: dos `Spool::new` son
            // dos registros de emisión que no se ven. Este de aquí barre Y es el
            // que se le instala al engine unas líneas más abajo, que es de donde
            // lo saca `sync.plan` y el cierre de cada conexión.
            let spool = norte_core::sync::Spool::new(norte_core::connect::config_dir());
            match spool.sweep().await {
                Ok(r) if r.removed == 0 && r.is_clean() => {}
                Ok(r) if r.is_clean() => {
                    eprintln!("barridos {} planes de sync huérfanos", r.removed);
                }
                Ok(r) => eprintln!(
                    "aviso: barridos {} planes de sync huérfanos y {} no se dejaron borrar en {}",
                    r.removed,
                    r.failed,
                    spool.dir().display()
                ),
                Err(e) => eprintln!("aviso: no se pudo barrer {}: {e}", spool.dir().display()),
            }
            // policy.toml: ausente = sin reglas = un agente DENTRO de scope
            // aún deniega (fail-closed, `no-rule`). docs/policy-example.toml
            // trae el punto de partida (`action = "ask"`).
            let cfg = tokio::task::spawn_blocking(norte_core::PolicyConfig::load)
                .await
                .context("carga de policy.toml")?
                .context("policy.toml inválido")?;
            let scopes = norte_core::ScopeRegistry::new();
            let approvals = std::sync::Arc::new(DaemonApprovalResolver::default());
            let engine = Engine::with_journal(std::sync::Arc::new(journal)).with_policy(
                std::sync::Arc::new(norte_core::ScopedPolicy::new(scopes.clone(), cfg)),
                std::sync::Arc::clone(&approvals) as _,
            );
            // Índice de búsqueda (M4, ADR 0034): junto al journal en config_dir.
            // Si no abre, se sigue sin él (index.* → Unsupported, fail-closed).
            let index_path = norte_core::connect::config_dir().join("index.db");
            let engine = match norte_core::Index::open(&index_path).await {
                Ok(idx) => engine.with_index(std::sync::Arc::new(idx)),
                Err(e) => {
                    eprintln!("aviso: índice no disponible ({e}); index.* dará Unsupported");
                    engine
                }
            };
            // EL spool, el mismo que acaba de barrer: sin él `sync.plan`
            // responde `Unsupported` (un plan que no se puede retener tampoco se
            // puede aplicar).
            engine.set_spool(spool);
            engine.register_provider(Arc::new(LocalProvider::os_root()) as Arc<dyn Provider>);
            apply_archive_limits(&engine).await?;
            engine.set_connector(std::sync::Arc::new(
                norte_core::connect::ConnectionManager::new(norte_core::connect::config_dir()),
            ));
            // IA (M4-IA, ADR 0031): opt-in. Sin [ai], sin proveedor o con
            // config rota el engine degrada (ai.* → Unsupported / gate
            // PolicyDenied); jamás aborta el arranque del daemon.
            match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
                Ok(Ok(config)) => {
                    if let Some(pcfg) = config.rename_provider_config().cloned() {
                        match norte_core::ai::resolve_and_build(
                            &pcfg,
                            norte_core::connect::config_dir(),
                        )
                        .await
                        {
                            Ok(provider) => engine.set_ai_provider(provider),
                            Err(e) => eprintln!(
                                "aviso: proveedor de IA no disponible ({e}); ai.* dará Unsupported"
                            ),
                        }
                    }
                    // Embeddings (M4-IA-2): proveedor propio, opt-in igual.
                    if let Some(w) = norte_core::ai::install_embed_provider(&engine, &config).await
                    {
                        eprintln!("{w}");
                    }
                    engine.set_ai_config(config);
                }
                Ok(Err(e)) => eprintln!("aviso: [ai] inválido ({e}); ai.* dará Unsupported"),
                Err(e) => eprintln!("aviso: carga de [ai] falló ({e}); ai.* dará Unsupported"),
            }
            let daemon = Daemon::bind_with_policy(
                std::sync::Arc::new(engine),
                scopes,
                approvals,
                DaemonConfig {
                    socket_path: socket,
                    idle_timeout: (idle_timeout > 0)
                        .then(|| std::time::Duration::from_secs(idle_timeout)),
                    ..DaemonConfig::default()
                },
            )
            .await
            .context("no se pudo enlazar el daemon")?;
            eprintln!(
                "{}",
                norte_i18n::ta(
                    "cli-daemon-listening",
                    &[("socket", &daemon.socket_path().display().to_string())]
                )
            );
            // Ctrl-C/SIGTERM = shutdown graceful (las tasks terminan);
            // la SEGUNDA señal escala a hard (cancela tasks) — sin ella,
            // una task colgada solo moriría con SIGKILL (M4 rust-reviewer).
            // El registro va ANTES del spawn: si falla, error visible, no
            // un panic tragado dentro de un task (M5).
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .context("no se pudo registrar SIGTERM")?;
            let shutdown = daemon.shutdown_token();
            let hard = daemon.hard_shutdown_token();
            tokio::spawn(async move {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
                shutdown.cancel();
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = sigterm.recv() => {}
                }
                eprintln!("{}", norte_i18n::t("cli-daemon-hard-shutdown"));
                hard.cancel();
            });
            daemon.run().await.context("el daemon terminó con error")?;
            Ok(ExitCode::SUCCESS)
        }
        DaemonCmd::Stop { socket, hard } => {
            // La resolución del path por defecto puede sondear el FS
            // (regla 2): fuera del hilo del runtime.
            let socket = match socket {
                Some(s) => s,
                None => tokio::task::spawn_blocking(|| default_socket_path(None))
                    .await
                    .context("resolución del socket")?,
            };
            let mut client = Client::connect(&socket)
                .await
                .context("no hay daemon escuchando en el socket")?;
            client
                .initialize(norte_proto::methods::ClientInfo {
                    name: "norte-cli".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                })
                .await
                .context("initialize")?;
            let _: norte_proto::methods::DaemonShutdownResult = client
                .call(
                    norte_proto::methods::DAEMON_SHUTDOWN,
                    &norte_proto::methods::DaemonShutdownParams { graceful: !hard },
                )
                .await
                .context("daemon.shutdown")?;
            eprintln!("{}", norte_i18n::t("cli-daemon-stopped"));
            Ok(ExitCode::SUCCESS)
        }
    }
}

/// Schemes remotos que la CLI enruta por URL. ALLOWLIST explícita: un path
/// local puede llamarse legalmente `a://b` (o `./x://y`) y debe seguir
/// siendo un fichero — solo lo que empieza EXACTAMENTE por estos prefijos se
/// trata como URL remota.
const REMOTE_SCHEMES: [&str; 3] = ["sftp://", "ftp://", "s3://"];

/// ¿El arg es una URL de archivo-como-directorio (ADR 0018)? Exige la forma
/// completa `<formato>+<scheme>://…` con formato de la whitelist de proto
/// (la reserva normativa garantiza que ningún provider legítimo empieza
/// así, test abajo): un path local raro tipo `zip+dir/sub://y` sigue
/// siendo nativo.
///
/// Delegado en `norte_proto::scheme_archive_format` (longest-match, #55) en
/// vez de reimplementar la gramática con `split_once('+')`: un token puede
/// contener `+` propio (`tar+gz`), y duplicar la whitelist aquí divergiría
/// en cuanto proto gane un formato compuesto nuevo.
fn is_archive_url(s: &str) -> bool {
    let Some((scheme, _)) = s.split_once("://") else {
        return false;
    };
    let Some(fmt) = norte_proto::scheme_archive_format(scheme) else {
        return false;
    };
    let inner = &scheme[fmt.len() + 1..];
    !inner.is_empty() && !inner.contains('/')
}

/// #95: el daemon honra `[archive]` de norte.toml (capa usuario) — antes
/// servía con los defaults compilados y ni operador ni policy podían bajar
/// los límites anti-bomba para agentes. Fail-loud: un norte.toml roto
/// aborta el arranque (mismo criterio que policy.toml).
#[cfg(unix)]
async fn apply_archive_limits(engine: &Engine) -> anyhow::Result<()> {
    if let Some(limits) =
        tokio::task::spawn_blocking(norte_core::archive_config::load_archive_limits)
            .await
            .context("carga de norte.toml")?
            .context("norte.toml inválido ([archive])")?
    {
        engine.set_archive_limits(limits);
    }
    Ok(())
}

/// `norte ai rename`: sugiere un rename por lote REVISABLE (M4-A2, ADR
/// 0031). Embebido: construye un engine con el proveedor de `[ai]`, pide el
/// plan (el gate opt-in/local-only/denied-paths corta ANTES de que ningún
/// nombre salga), lo IMPRIME y confirma antes de aplicar. Aplicar = N
/// `move_` gobernados (journal + undo + policy) — el plan es el producto.
async fn ai_cmd(cmd: AiCmd) -> anyhow::Result<ExitCode> {
    let AiCmd::Rename {
        dir,
        instruction,
        yes,
    } = cmd;
    let dir = vpath(&dir)?;

    // #167: este subcomando NO pasa por `make_backend` —arma su propio engine—
    // y renombra un directorio entero con los nombres que propuso un MODELO.
    // Es, de todos los caminos embebidos, el que más falta le hace quedar
    // registrado, así que lleva journal como los demás. Perezoso como los demás
    // también (#177): planificar es leer, y leer no le quita el journal a
    // nadie; el lock se toma abajo, a un paso de renombrar.
    let engine = norte_core::embedded::engine_in(&norte_core::connect::config_dir());
    // Este brazo no pasa por `run`, así que instala el suyo — ver
    // `AvisoDeJournalPorStderr`.
    engine.set_journal_warning_sink(Arc::new(AvisoDeJournalPorStderr));
    engine.register_provider(Arc::new(LocalProvider::os_root()) as Arc<dyn Provider>);
    engine.set_connector(Arc::new(norte_core::connect::ConnectionManager::new(
        norte_core::connect::config_dir(),
    )));

    let config = tokio::task::spawn_blocking(norte_core::ai::AiConfig::load)
        .await
        .context("carga de [ai]")?
        .context("[ai] inválido en norte.toml")?;
    let Some(pcfg) = config.rename_provider_config().cloned() else {
        anyhow::bail!(
            "sin proveedor de IA para el rename: define [ai.providers.<n>] y \
             rename_provider en norte.toml (ADR 0031)"
        );
    };
    // El core resuelve el secreto (env → keyring → age) y construye el
    // proveedor; el CLI no toca norte-connect ni ve la clave (regla 10).
    let provider = norte_core::ai::resolve_and_build(&pcfg, norte_core::connect::config_dir())
        .await
        .map_err(|e| anyhow::anyhow!("proveedor de IA: {e}"))?;
    engine.set_ai_provider(provider);
    engine.set_ai_config(config);

    let plan = engine
        .ai_rename_plan(&dir, &instruction)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    if plan.entries.is_empty() {
        println!("{}", norte_i18n::t("cli-ai-rename-empty"));
        return Ok(ExitCode::SUCCESS);
    }
    println!("{}", norte_i18n::t("cli-ai-rename-plan"));
    // Nombres controlados por el MODELO: enmascarar hazards de terminal
    // (bidi/invisibles → �) y MARCAR el enmascarado, como TUI/GUI. Un reply
    // UTF-8 válido puede traer RLO y spoofear el prompt de confirmación.
    let masked = |bytes: &[u8]| {
        let (texto, hostil) = norte_frontend::display_name(bytes);
        format!("{}{texto}", if hostil { "!" } else { "" })
    };
    for e in &plan.entries {
        println!(
            "  {} → {}",
            masked(e.from.as_bytes()),
            masked(e.to.as_bytes())
        );
    }

    // El journal se abre AQUÍ, antes de preguntar y antes de renombrar, y no en
    // el primer rename: lo que se está decidiendo es si un modelo renombra un
    // directorio entero, y «esto no se va a poder deshacer» es parte de la
    // pregunta, no una nota a pie después del sí. FUERA del `if !yes`: con
    // `--yes` no hay pregunta que completar, pero sigue habiendo un humano (o
    // un script cuyo log alguien lee) al que le toca enterarse, y ese es
    // justamente el camino donde nadie está mirando la pantalla.
    //
    // El motivo lo acaba de decir el sink de stderr; aquí va la consecuencia.
    if !engine.ensure_journal().await {
        eprintln!("norte: {}", norte_i18n::t("cli-ai-rename-unjournalled"));
    }

    if !yes {
        use std::io::Write as _;
        eprint!("{} ", norte_i18n::t("cli-ai-rename-confirm"));
        std::io::stderr().flush().ok();
        let mut line = String::new();
        std::io::stdin().read_line(&mut line).ok();
        let ans = line.trim().to_ascii_lowercase();
        if ans != "y" && ans != "s" {
            println!("{}", norte_i18n::t("cli-ai-rename-abort"));
            return Ok(ExitCode::SUCCESS);
        }
    }

    // Aplica cada entrada como un `move_` del engine, que desde #167 lleva
    // journal (y por tanto undo) salvo que otro proceso tenga el lock. La
    // policy NO: este engine embebido no instala ninguna y gatea con `AllowAll`
    // — quien decide aquí es el humano que acaba de decir que sí al plan.
    let mut ok = 0usize;
    for e in &plan.entries {
        let to = dir.join(e.to.clone());
        let from = dir.join(e.from.clone());
        match engine.move_(&from, &to).await {
            Ok(task) => match task.join().await {
                TaskState::Completed => ok += 1,
                other => eprintln!(
                    "norte: {} → {}: {other:?}",
                    masked(e.from.as_bytes()),
                    masked(e.to.as_bytes())
                ),
            },
            Err(err) => eprintln!(
                "norte: {} → {}: {err}",
                masked(e.from.as_bytes()),
                masked(e.to.as_bytes())
            ),
        }
    }
    println!(
        "{}",
        norte_i18n::ta("cli-ai-rename-done", &[("n", &ok.to_string())])
    );
    Ok(ExitCode::SUCCESS)
}

/// `norte gc`: barre staging `.norte-partial` huérfano (#11, ADR 0012).
/// Solo embebido: el wire no expone (aún) el GC — con `--daemon` el error
/// es accionable, no un `Unsupported` seco.
async fn gc_cmd(
    backend: &Backend,
    daemon: bool,
    path: &std::path::Path,
    older_than_hours: u64,
) -> anyhow::Result<ExitCode> {
    let dir = vpath(path)?;
    let older = std::time::Duration::from_secs(older_than_hours.saturating_mul(3600));
    match backend.gc_partials(&dir, older).await {
        Ok(n) => {
            println!(
                "{}",
                norte_i18n::ta(
                    "cli-gc-result",
                    &[("n", &n.to_string()), ("dir", &dir.display_lossy())],
                )
            );
            Ok(ExitCode::SUCCESS)
        }
        Err(norte_proto::Error::Unsupported) if daemon => {
            anyhow::bail!("{}", norte_i18n::t("cli-gc-remote-unsupported"))
        }
        Err(e) => Err(anyhow::anyhow!("{e}")),
    }
}

fn vpath(path: &std::path::Path) -> anyhow::Result<VPath> {
    // Una URL remota va por el parser wire; todo lo demás es un path NATIVO
    // local (bytes, jamás forzados a UTF-8 — un arg no-UTF8 no puede ser URL
    // y cae al camino nativo).
    if let Some(s) = path.to_str()
        && (REMOTE_SCHEMES.iter().any(|p| s.starts_with(p)) || is_archive_url(s))
    {
        reject_inline_password(s)?;
        return VPath::parse(s).with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", s)]));
    }
    norte_vfs_local::vpath_from_native(path)
        .with_context(|| format!("path no representable: {}", path.display()))
}

/// Rechaza `user:pass@host` en una URL ANTES de que entre a `VPath::parse`
/// (que la aceptaría) y por tanto a spans/errores: mensaje ESTÁTICO, sin
/// ecoar la URL (regla 10). El parser de conexiones la rechazaría después,
/// pero para entonces ya habría tocado logs.
fn reject_inline_password(url: &str) -> anyhow::Result<()> {
    let after_scheme = url.split_once("://").map_or(url, |(_, r)| r);
    let authority = after_scheme.split('/').next().unwrap_or(after_scheme);
    if let Some((userinfo, _)) = authority.rsplit_once('@')
        && userinfo.contains(':')
    {
        anyhow::bail!(norte_i18n::t("cli-inline-password"));
    }
    Ok(())
}

/// Flujo TOFU interactivo (ADR 0015 D/G): ante un `Error::HostKeyUnknown`
/// muestra la huella, pide confirmación por el terminal y, si el usuario
/// acepta, la registra (el core re-verifica anti-TOCTOU) y devuelve `true`
/// (reintentar la operación). Errores que no son TOFU → `false` (el caller
/// reporta el original). Sin terminal NO se confía nada: instrucciones y error.
async fn tofu_confirm(backend: &Backend, err: &norte_proto::Error) -> anyhow::Result<bool> {
    use std::io::IsTerminal;
    let norte_proto::Error::HostKeyUnknown {
        host,
        port,
        algo,
        fingerprint,
    } = err
    else {
        return Ok(false);
    };
    let port_shown = port.unwrap_or(22).to_string();
    eprintln!(
        "{}",
        norte_i18n::ta(
            "cli-hostkey-unknown",
            &[("host", host.as_str()), ("port", port_shown.as_str())]
        )
    );
    eprintln!(
        "{}",
        norte_i18n::ta(
            "cli-hostkey-fingerprint",
            &[
                ("algo", algo.as_str()),
                ("fingerprint", fingerprint.as_str())
            ]
        )
    );
    if !std::io::stdin().is_terminal() {
        anyhow::bail!(norte_i18n::t("cli-hostkey-noninteractive"));
    }
    eprint!("{} ", norte_i18n::t("cli-hostkey-prompt"));
    // stdin es bloqueante: fuera del reactor (regla 2).
    let line = tokio::task::spawn_blocking(|| {
        let mut s = String::new();
        std::io::stdin().read_line(&mut s).map(|_| s)
    })
    .await
    .context(norte_i18n::t("cli-confirm-read"))??;
    let yes = matches!(
        line.trim().to_ascii_lowercase().as_str(),
        "s" | "si" | "sí" | "y" | "yes"
    );
    if !yes {
        anyhow::bail!(norte_i18n::t("cli-hostkey-refused"));
    }
    backend
        .trust_host_key(host, *port, algo, fingerprint)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    eprintln!("{}", norte_i18n::t("cli-hostkey-trusted"));
    Ok(true)
}

/// `norte connect <nombre|url>`: establece la conexión (disparando el flujo
/// TOFU si es el primer contacto) y confirma. El valor duradero es el
/// registro de la host key + la validación de credenciales.
async fn connect_cmd(backend: &Backend, target: &str, daemon: bool) -> anyhow::Result<ExitCode> {
    if daemon {
        // La resolución por nombre lee el config LOCAL; contra un daemon
        // remoto la semántica cambia — se difiere (mínimo viable, ADR 0015 G).
        anyhow::bail!(norte_i18n::t("cli-connect-daemon-unsupported"));
    }
    let url = if target.contains("://") {
        target.to_string()
    } else {
        // Nombre de connections.toml → su URL (el core la resuelve).
        norte_core::connect::named_url(&norte_core::connect::config_dir(), target)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
            .context(norte_i18n::t("cli-connect-failed"))?
    };
    reject_inline_password(&url)?;
    let root = VPath::parse(&url)
        .with_context(|| norte_i18n::ta("cli-invalid-url", &[("url", url.as_str())]))?;
    // capabilities fuerza el establecimiento por el camino normal del engine.
    let result = match backend.capabilities(&root).await {
        Err(e) if tofu_confirm(backend, &e).await? => backend.capabilities(&root).await,
        other => other,
    };
    result
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-connect-failed"))?;
    println!(
        "{}",
        norte_i18n::ta("cli-connect-ok", &[("target", target)])
    );
    Ok(ExitCode::SUCCESS)
}

/// Traduce `--criteria` a un [`norte_proto::methods::CompareCriteria`].
///
/// Vacío = el default del wire (tamaño y fecha, sin hash — ver el doctest de
/// `FsCompareParams`). No vacío = EXACTAMENTE la lista pedida: `--criteria
/// hash` a secas enciende solo `hash` y apaga `size`/`mtime`, para que "quiero
/// nada más que el hash" tenga el efecto obvio en la petición aunque el core
/// (ADR 0048) solo lo corra sobre las parejas que los rungs baratos ya dieron
/// por iguales.
fn parse_compare_criteria(
    names: &[String],
) -> anyhow::Result<norte_proto::methods::CompareCriteria> {
    if names.is_empty() {
        return Ok(norte_proto::methods::CompareCriteria::default());
    }
    let mut criteria = norte_proto::methods::CompareCriteria {
        size: false,
        mtime: false,
        hash: false,
    };
    for name in names {
        match name.as_str() {
            "size" => criteria.size = true,
            "mtime" => criteria.mtime = true,
            "hash" => criteria.hash = true,
            other => anyhow::bail!(
                "--criteria: criterio desconocido \"{}\"",
                other.escape_debug()
            ),
        }
    }
    Ok(criteria)
}

/// `norte compare`: `fs.compare` y su veredicto en el código de salida.
///
/// # Por qué el veredicto va en el código
/// Es la pregunta «¿funcionó la copia?», y quien la hace suele ser un script.
/// `diff` contesta así desde siempre y no hay nada que mejorar en esa
/// convención: 0 iguales, 1 difieren, y un tercer código para «no se pudo
/// saber» que es el que de verdad importa aquí — una comparación INCOMPLETA
/// que contestara 0 sería exactamente el fallo que este comando existe para
/// no cometer.
async fn compare_cmd(
    backend: &Backend,
    a: &std::path::Path,
    b: &std::path::Path,
    json: bool,
    criteria: &[String],
    max_depth: Option<u32>,
    mtime_tolerance_ms: Option<u32>,
) -> anyhow::Result<ExitCode> {
    let left = vpath(a)?;
    let right = vpath(b)?;
    let params = norte_proto::methods::FsCompareParams {
        left,
        right,
        criteria: parse_compare_criteria(criteria)?,
        max_depth,
        // 2000 ms es el default declarado por `FsCompareParams` (la regla
        // FAT, ADR 0048; ver su doctest: `mtime_tolerance_ms == 2000`). Se
        // repite el número aquí porque el tipo no deriva `Default` y la
        // constante que lo fija en el proto es privada — no hay un
        // `FsCompareParams::default()` que reutilizar.
        mtime_tolerance_ms: mtime_tolerance_ms.unwrap_or(2000),
        // `Backend::compare` rechaza `follow_symlinks: true`, y este comando
        // no tiene motivo para diferir de `sync_plan`, que rechaza los dos.
        follow_symlinks: false,
        descend_orphans: None,
    };

    let (task, mut rx) = backend
        .compare(params)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-compare-failed"))?;

    // Nombres del árbol del OTRO lado, que este proceso no controla: MARCAR
    // el enmascarado, igual que `ai_cmd` — un nombre remoto puede traer RLO y
    // spoofear la salida. `cells_for` ya enmascaró `RowFace::name` con
    // `display_name_with` (rule 1); esto solo añade el `!` de `ai_cmd` sobre
    // el `hostile` que esa llamada ya calculó — no un segundo enmascarado por
    // separado, que divergiría el día que este comando gane una
    // reinterpretación (#57) y alguien olvide threadearla también aquí.
    let mark = |face: &norte_frontend::compare::RowFace| {
        format!("{}{}", if face.hostile { "!" } else { "" }, face.name)
    };

    // Se decide fila a fila mientras se drena — jamás se coleccionan (la
    // rustdoc de `ComparePane` explica lo que cuesta retener un millón de
    // filas, y este comando no tiene motivo para retener ninguna).
    let mut any_different = false;
    while let Some(batch) = rx.recv().await {
        for row in &batch.rows {
            if row.verdict != norte_proto::methods::CompareVerdict::Same {
                any_different = true;
            }
            if json {
                // Forma wire (lossless); --json no traduce ni enmascara —
                // un consumidor de script decodifica con el mismo códec que
                // `norte ls --json`.
                println!("{}", serde_json::to_string(row)?);
            } else {
                let cells = norte_frontend::compare::cells_for(row, None, None);
                let glyph = norte_frontend::compare::verdict_glyph(row.verdict);
                let left_name = cells.left.as_ref().map_or_else(String::new, mark);
                let right_name = cells.right.as_ref().map_or_else(String::new, mark);
                println!("{glyph} {left_name}\t{right_name}");
            }
        }
    }

    match task.join().await {
        TaskState::Completed => Ok(ExitCode::from(u8::from(any_different))),
        other => {
            eprintln!(
                "norte: {}",
                norte_i18n::ta(
                    "cli-compare-incomplete",
                    &[("state", &format!("{other:?}"))],
                )
            );
            Ok(ExitCode::from(2))
        }
    }
}

/// `norte sync`: planifica, enseña, pregunta, aplica — TODO en una conexión.
///
/// # Por qué una sola invocación
/// Un plan aprobado se retiene POR CONEXIÓN, en un registro en memoria que
/// nace vacío, y `sync.apply` no lleva nada más que el `plan_hash`. Un CLI que
/// planease en un proceso y aplicara en otro no podría funcionar ni queriendo:
/// el registro del segundo no conoce ese hash. Así que la pregunta se hace con
/// la conexión viva, y `--dry-run` es esta misma función sin la segunda mitad.
///
/// # Esta mitad (tarea 2)
/// Planifica, drena el stream por [`norte_frontend::sync::SyncState`] —el
/// ÚNICO sitio donde los pasos se cuadran contra los contadores del cierre—,
/// enseña el plan entero, y para. La tarea 3 añade, ANTES del `return` final,
/// la resolución del journal, la pregunta y el apply: hasta que esa aterrice,
/// ningún camino de esta función llama a `Backend::sync_apply`, así que ni
/// `--dry-run` ni su ausencia pueden mutar nada.
async fn sync_cmd(
    backend: &Backend,
    source: &std::path::Path,
    dest: &std::path::Path,
    mode: SyncModeArg,
    dry_run: bool,
    criteria: &[String],
    mtime_tolerance_ms: Option<u32>,
) -> anyhow::Result<ExitCode> {
    let source = vpath(source)?;
    let dest = vpath(dest)?;

    // `SyncCompareOptions` sí deriva un `Default` de verdad (a diferencia de
    // `FsCompareParams` en `compare_cmd`), así que no hay un 2000 mágico que
    // repetir aquí.
    let mut compare = norte_proto::methods::SyncCompareOptions {
        criteria: parse_compare_criteria(criteria)?,
        ..norte_proto::methods::SyncCompareOptions::default()
    };
    if let Some(ms) = mtime_tolerance_ms {
        compare.mtime_tolerance_ms = ms;
    }

    let params = norte_proto::methods::SyncPlanParams {
        source,
        dest,
        mode: mode.into(),
        compare,
        // "ausente = del llamante no es" — se deja en su default (Copy), como
        // pide la tarea.
        on_unknown: norte_proto::methods::OnUnknown::default(),
        include: None,
    };

    let (task, mut rx) = backend
        .sync_plan(params)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-sync-failed"))?;

    // El ÚNICO sitio donde los pasos se cuadran contra `SyncPlanDone::counts`
    // es `SyncState`; montar un `SyncPlan` a mano sería una segunda ocasión de
    // olvidar esa comprobación (la razón de ser de esta tarea).
    let mut state = norte_frontend::sync::SyncState::default();
    while let Some(event) = rx.recv().await {
        match event {
            norte_core::sync::SyncPlanEvent::Steps(batch) => {
                state.on_steps(batch);
            }
            norte_core::sync::SyncPlanEvent::Done(done) => {
                state.on_plan_done(done);
            }
        }
    }

    // El canal se cierra cuando la Task termina, así que este `join` no
    // espera de más. Se exige AMBAS cosas: que el estado haya cerrado
    // (`sync.plan_done` llegó) Y que la Task terminara `Completed`. Un canal
    // que se cierra con el diálogo aún en `Planning` — la Task murió,
    // canceló, o el buffer de este proceso se llenó y el enrutado cerró el
    // feed (ver la rustdoc de `Backend::sync_plan`) — es exactamente el "no
    // se pudo saber" que no puede confundirse con "sin diferencias": sin
    // `sync.plan_done` no hay `plan_hash` y no hay nada que aprobar.
    let task_state = task.join().await;
    let plan = match state {
        norte_frontend::sync::SyncState::Ready(plan) if task_state == TaskState::Completed => plan,
        _ => {
            eprintln!(
                "norte: {}",
                norte_i18n::ta(
                    "cli-sync-incomplete",
                    &[("state", &format!("{task_state:?}"))],
                )
            );
            return Ok(ExitCode::from(2));
        }
    };

    if plan.steps().is_empty() {
        println!("{}", norte_i18n::t("cli-sync-empty"));
        return Ok(ExitCode::SUCCESS);
    }

    println!("{}", norte_i18n::t("cli-sync-plan"));
    // Nombres que este proceso no controla del todo (el destino puede
    // deletrear una entrada distinto del origen, #152): MARCAR el
    // enmascarado, igual que `ai_cmd` y `compare_cmd`. `render_step` ya
    // enmascaró `RelDisplay::text` (`display_name`, regla 1); esto solo añade
    // el `!` sobre el `hostile` que esa llamada ya calculó — no un segundo
    // enmascarado por separado.
    let mark = |d: &norte_frontend::sync::RelDisplay| {
        format!("{}{}", if d.hostile { "!" } else { "" }, d.text)
    };
    for step in plan.steps() {
        let cells = norte_frontend::sync::render_step(step, plan.dest_trash(), None);
        let rel = mark(&cells.rel);
        let dest_suffix = cells
            .dest_rel
            .as_ref()
            .map_or_else(String::new, |d| format!(" → {}", mark(d)));
        println!(
            "{}{} {rel}{dest_suffix}",
            cells.glyphs.kind, cells.glyphs.undo
        );
    }
    for line in plan.summary_lines(norte_i18n::active()) {
        println!("{line}");
    }

    if dry_run {
        return Ok(ExitCode::from(1));
    }
    // La tarea 3 inserta AQUÍ, antes de este `return`: resolver el journal,
    // preguntar, aplicar e informar. Hasta que aterrice, `norte sync` sin
    // `--dry-run` para exactamente donde para `--dry-run` — imprimir el plan
    // es TODO lo que hace esta función, y no hay ninguna llamada a
    // `Backend::sync_apply` en este archivo. El aviso es a stderr para que un
    // script que solo mire el código de salida (1, igual que `--dry-run`) no
    // se quede creyendo que aplicó algo.
    eprintln!("norte: {}", norte_i18n::t("cli-sync-not-yet-applied"));
    Ok(ExitCode::from(1))
}

async fn ls(
    backend: &Backend,
    path: &std::path::Path,
    json: bool,
    attrs: &[String],
) -> anyhow::Result<ExitCode> {
    let target = vpath(path)?;
    // Validación ANTES del backend (review #108-b2 MAJOR): el daemon
    // rechaza ids malformados/sobre-tope con -32602 pero el backend
    // embebido FILTRA — sin este gate el mismo comando se comportaría
    // distinto según el transporte. `escape_debug`: el id es entrada
    // del usuario pero puede venir de un script hostil.
    if attrs.len() > norte_proto::ATTRS_MAX_REQUEST {
        anyhow::bail!(
            "--attrs: at most {} ids per call",
            norte_proto::ATTRS_MAX_REQUEST
        );
    }
    if let Some(bad) = attrs.iter().find(|id| !norte_proto::is_valid_attr_id(id)) {
        anyhow::bail!("--attrs: malformed id \"{}\"", bad.escape_debug());
    }
    let (mut entries, skipped): (Vec<Entry>, Option<u64>) =
        match backend.list_with_skipped_attrs(&target, attrs).await {
            // Primer contacto TOFU: confirmar y reintentar UNA vez.
            Err(e) if tofu_confirm(backend, &e).await? => {
                backend.list_with_skipped_attrs(&target, attrs).await
            }
            other => other,
        }
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-list-failed"))?;
    // #93: el contenedor omitió entradas de su índice — el listado NO es todo
    // lo que el archivo contiene. A stderr (no contamina stdout ni --json).
    if let Some(n) = skipped.filter(|&n| n > 0) {
        eprintln!(
            "{}",
            norte_i18n::ta("cli-ls-skipped", &[("n", &n.to_string())])
        );
    }
    // #52: el listado local es lazy (size/mtime_ms en None). `ls` es un
    // comando de UNA sola pasada (no hay foco que hidrate luego, como en la
    // TUI): se hidrata aquí, serial, ANTES de imprimir — restaura el output
    // pre-#52 (texto y --json) al costo pre-#52 (un stat por File) — solo para
    // Files: Dir/Symlink emiten null en --json (su mtime no es contrato de
    // `ls`). Un stat fallido deja `None` (columna/campo vacíos): jamás
    // aborta el listado.
    for e in &mut entries {
        if e.kind == EntryKind::File
            && (e.size.is_none() || e.mtime_ms.is_none())
            && let Ok(st) = backend.stat(&e.path).await
        {
            e.size = e.size.or(st.size);
            e.mtime_ms = e.mtime_ms.or(st.mtime_ms);
        }
    }
    if json {
        // Forma wire (lossless); el consumidor decodifica con el codec.
        serde_json::to_writer_pretty(std::io::stdout().lock(), &entries)
            .context(norte_i18n::t("cli-serialize-failed"))?;
        println!();
    } else {
        use std::fmt::Write as _;
        for e in &entries {
            let marker = match e.kind {
                EntryKind::Dir => "d",
                EntryKind::File => "-",
                EntryKind::Symlink => "l",
                EntryKind::Other => "?",
            };
            let size = e.size.map_or_else(String::new, |s| s.to_string());
            let mut line = format!("{marker}\t{size}\t{}", e.path.display_lossy());
            // Attrs pedidos (#108 bloque 2), en el orden de la petición;
            // ausente = columna que no se pinta (jamás un 0 inventado).
            for id in attrs {
                if let Some(v) = e.attrs.get(id) {
                    let _ = write!(line, "\t{id}={}", render_attr_value(v));
                }
            }
            println!("{line}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// Valor de attr para el `ls` humano. Texto y bytes son de TERCEROS:
/// `escape_debug` neutraliza controles, RTL e invisibles; los bytes pasan por
/// lossy ANTES (regla 1: la pérdida es explícita y solo de presentación —
/// `--json` conserva la forma wire exacta).
fn render_attr_value(v: &norte_proto::AttrValue) -> String {
    use norte_proto::AttrValue as V;
    match v {
        V::Uint(n) => n.to_string(),
        V::Int(n) | V::TimeMs(n) => n.to_string(),
        V::Bool(b) => b.to_string(),
        V::Text(s) => s.escape_debug().to_string(),
        V::Bytes(b) => String::from_utf8_lossy(b).escape_debug().to_string(),
        V::Unknown => "?".to_owned(),
    }
}

/// Corre una Task pintando progreso en stderr; Ctrl-C cancela cooperativamente
/// (la task deja destino limpio o `.norte-partial`, regla dura 3).
async fn run_task(task: TaskRef, show_bytes: bool) -> ExitCode {
    let canceller = task.canceller();
    let sig = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            eprintln!("\n{}", norte_i18n::t("cli-cancelling"));
            canceller.cancel();
        }
    });

    let mut rx = task.progress();
    loop {
        let snap = rx.borrow_and_update().clone();
        render(&snap, show_bytes);
        if snap.state.is_terminal() {
            break;
        }
        if rx.changed().await.is_err() {
            break;
        }
    }
    sig.abort();
    let final_state = rx.borrow().state.clone();
    eprintln!();
    match final_state {
        TaskState::Completed => ExitCode::SUCCESS,
        TaskState::Cancelled => {
            eprintln!("{}", norte_i18n::t("cli-cancelled-clean"));
            ExitCode::from(EXIT_CANCELLED)
        }
        TaskState::Failed { error } => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-final-error", &[("error", &error.to_string())])
            );
            ExitCode::FAILURE
        }
        other => {
            eprintln!(
                "{}",
                norte_i18n::ta("cli-unexpected-state", &[("state", &format!("{other:?}"))])
            );
            ExitCode::FAILURE
        }
    }
}

fn render(p: &norte_proto::TaskProgress, show_bytes: bool) {
    let entries = match p.entries_total {
        Some(t) => format!("{}/{t}", p.entries_done),
        None => format!("{}/?", p.entries_done),
    };
    if show_bytes {
        let bytes = match p.bytes_total {
            Some(t) if t > 0 => {
                let pct = p.bytes_done.saturating_mul(100) / t;
                format!("{} / {t} bytes ({pct}%)", p.bytes_done)
            }
            _ => format!("{} bytes", p.bytes_done),
        };
        eprint!("\r{bytes} — {entries} entradas   ");
    } else {
        eprint!("\r{entries} entradas   ");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reserva normativa de ADR 0018: ningún scheme remoto de la allowlist
    /// puede empezar por `<formato>+` — el registro de formatos manda.
    #[test]
    fn remote_schemes_respetan_la_reserva_de_formatos() {
        for scheme in REMOTE_SCHEMES {
            for format in norte_proto::ARCHIVE_FORMATS {
                assert!(
                    !scheme.starts_with(&format!("{format}+")),
                    "{scheme} invade el namespace del formato {format}"
                );
            }
        }
    }

    /// ADR 0046: every verdict that is not `Intact` has something to say in
    /// both locales, and the "unknown format" one says the version WITHOUT
    /// accusing anyone. A missing Fluent key falls back to the key itself,
    /// which in this path would be the whole message the operator gets.
    #[test]
    fn cada_veredicto_no_certificado_tiene_su_mensaje_en_los_dos_idiomas() {
        use norte_core::{ChainStatus, JournalFormat};
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let unknown = norte_i18n::ta_in(
                lang,
                "cli-audit-chain-unknown-format",
                &[("declared", "999"), ("known", "1")],
            );
            assert!(unknown.contains("999"), "{lang:?}: {unknown}");
            assert!(!unknown.starts_with("cli-audit"), "{lang:?}: sin traducir");
            for key in [
                "cli-audit-chain-unverifiable-from",
                "cli-audit-chain-not-certified",
                "cli-audit-format-unreadable",
            ] {
                let msg = norte_i18n::ta_in(lang, key, &[("seq", "7")]);
                assert!(
                    !msg.starts_with("cli-audit"),
                    "{lang:?}/{key}: sin traducir"
                );
            }
        }
        // Y no panica con ninguna forma del veredicto (incluida la rama de
        // cierre en falso, que es lo que verá un veredicto futuro).
        for status in [
            ChainStatus::Broken { first_bad_seq: 3 },
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Version(999),
                known: norte_core::JOURNAL_FORMAT,
                first_unverifiable_seq: Some(1),
            },
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Unreadable,
                known: norte_core::JOURNAL_FORMAT,
                first_unverifiable_seq: None,
            },
        ] {
            report_chain_not_certified(&status, JournalFormat::Version(999));
        }
        // Y una rotura en un journal cuyo formato tampoco se puede leer dice
        // las DOS cosas: la rotura es verdad, y sin la salvedad no se puede
        // interpretar.
        report_chain_not_certified(
            &ChainStatus::Broken { first_bad_seq: 3 },
            JournalFormat::Unreadable,
        );
    }

    /// H3e: every `plugin-help-*` code must have a renderer arm AND a Fluent
    /// message in BOTH locales. A missing key falls back to the key ITSELF
    /// (`norte_i18n::ta_in`'s contract), and a missing arm falls back to the
    /// raw machine `detail` — both are silent in text mode, so they are pinned
    /// here instead.
    #[test]
    fn los_hallazgos_plugin_help_se_renderizan_en_los_dos_idiomas() {
        for code in [
            "plugin-help-truncated",
            "plugin-help-lossy",
            "plugin-help-empty",
            "plugin-help-bad-header",
            "plugin-help-foreign-command",
            "plugin-help-shadows-topic",
        ] {
            let key = format!("cli-doctor-detail-{code}");
            for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
                let rendered = norte_i18n::ta_in(
                    lang,
                    &key,
                    &[("id", "acme.ftp"), ("detail", "acme.ftp: fs.copy")],
                );
                assert_ne!(rendered, key, "{key} missing in {lang:?}");
                assert!(
                    rendered.contains("acme.ftp"),
                    "{key} drops the id: {rendered}"
                );
            }
            // …and the arm exists: the line is not the raw detail.
            let f = doctor::Finding {
                section: "plugins",
                severity: doctor::Severity::Warn,
                code,
                detail: "acme.ftp".to_owned(),
            };
            assert_ne!(
                doctor_finding_line(&f),
                f.detail,
                "{code} has no renderer arm"
            );
        }
    }

    #[test]
    fn urls_de_archivo_van_por_el_parser_wire() {
        let p = vpath(std::path::Path::new("zip+file:///tmp/a.zip/!/x")).expect("parsea");
        assert_eq!(p.scheme(), "zip+file");
        // Un path local que solo se PARECE (sin `://`) sigue siendo nativo.
        let p = vpath(std::path::Path::new("zip+file")).expect("nativo");
        assert_eq!(p.scheme(), "file");
        // Password inline en un compuesto remoto: mismo guard que siempre.
        // Directo contra el guard (el parse TAMBIÉN lo rechaza desde #46,
        // pero este test protege la defensa en profundidad de la CLI).
        assert!(reject_inline_password("tar+sftp://u:pass@h/a.tar/!").is_err());
        assert!(vpath(std::path::Path::new("tar+sftp://u:pass@h/a.tar/!")).is_err());
        // Paths locales patológicos que se PARECEN: nativos, no URL.
        for nativo in ["zip+dir/sub://y", "tar+xz", "zip+://x"] {
            assert!(!is_archive_url(nativo), "{nativo} debe ser nativo");
        }
    }

    /// #55: `tar+gz` es un TOKEN COMPUESTO en la whitelist de proto —
    /// `is_archive_url` debe reconocerlo vía `scheme_archive_format`
    /// (longest-match), no reimplementando la gramática con `split_once('+')`
    /// (eso dejaría un interior huérfano tipo `gz+file` para casos con más de
    /// un nivel, y duplica una whitelist que ya vive en proto — regla 8).
    #[test]
    fn is_archive_url_reconoce_targz_compuesto() {
        assert!(is_archive_url("tar+gz+file://x"));
        assert!(is_archive_url("tar+gz+sftp://h/a.tgz/!/x"));
        // #56: anidado multi-capa también enruta por el parser wire.
        assert!(is_archive_url("zip+tar+file:///b.tar/!/i.zip/!/f"));
        // El wire completo enruta por el parser y compone el scheme real.
        let p = vpath(std::path::Path::new("tar+gz+file:///a.tgz/!/x")).expect("parsea");
        assert_eq!(p.scheme(), "tar+gz+file");
    }

    /// H1 (encoding review #108-b2): el render humano de attrs NEUTRALIZA
    /// texto de terceros — RTL override/ZWJ escapados, bytes no-UTF-8 por
    /// lossy+escape, jamás crudos en la terminal.
    #[test]
    fn render_attr_value_neutraliza_hostiles() {
        use norte_proto::AttrValue;
        // Los valores hostiles canónicos del MemProvider sintético.
        let texto = render_attr_value(&AttrValue::Text("\u{202e}atón\u{202c} a\u{200d}b".into()));
        assert!(!texto.contains('\u{202e}'), "RTL escapado: {texto}");
        assert!(!texto.contains('\u{200d}'), "ZWJ escapado: {texto}");
        assert!(texto.contains("\\u{202e}"), "visible como escape: {texto}");
        let bytes = render_attr_value(&AttrValue::Bytes(b"due\xf1o-\xff\xfe".to_vec()));
        // Lossy explícito: los bytes inválidos son U+FFFD visibles, el resto
        // legible, y jamás controles crudos.
        assert!(bytes.contains("due"), "parte legible conservada: {bytes}");
        assert!(bytes.contains('\u{fffd}'), "pérdida VISIBLE: {bytes}");
        assert!(!bytes.bytes().any(|b| b < 0x20), "sin controles crudos");
        // Un tab dentro del valor no inyecta columna: va escapado.
        let tab = render_attr_value(&AttrValue::Text("a\tb".into()));
        assert_eq!(tab, "a\\tb");
        assert_eq!(render_attr_value(&AttrValue::Uint(7)), "7");
        assert_eq!(render_attr_value(&AttrValue::Unknown), "?");
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
            frontend_program(None, "norte-gui"),
            std::path::PathBuf::from("norte-gui")
        );
    }
}
