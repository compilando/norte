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
            "could not run `{bin}` ({}) — install it with `cargo install --path crates/{bin}`",
            program.display()
        )))
    }
    #[cfg(not(unix))]
    {
        let status = cmd.status().with_context(|| {
            format!(
                "could not run `{bin}` ({}) — install it with `cargo install --path crates/{bin}`",
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
    about = "orthodox file manager — smoke-test CLI (M0)",
    // H3g: `help` is OUR subcommand (the help corpus, ADR 0040), not the
    // one clap generates to reprint its own `--help`. Without this, clap
    // aborts building the parser: «command name `help` is duplicated». Clap's
    // help stays where it always was — `norte --help`, `norte <cmd> --help`;
    // what is lost is `norte help <cmd>` as a synonym for that `--help`, and
    // the product documentation wants that name.
    disable_help_subcommand = true
)]
struct Cli {
    /// Operates against the daemon (starting it if needed) instead of the
    /// embedded core. Unix only (ADR 0011).
    #[arg(long, global = true)]
    daemon: bool,
    /// The daemon's socket (default: `$XDG_RUNTIME_DIR/norte/daemon.sock`)
    #[arg(long, global = true)]
    socket: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Lists a directory
    Ls {
        /// Directory to list
        path: PathBuf,
        /// JSON output (paths in wire form, lossless)
        #[arg(long)]
        json: bool,
        /// Provider attributes to request per entry, repeatable (e.g.
        /// `--attrs posix.mode`); the catalogue is published by `fs.capabilities`
        #[arg(long = "attrs", value_name = "ID")]
        attrs: Vec<String>,
    },
    /// Copies a file or directory (recursive), with progress and clean Ctrl-C
    Cp {
        /// Source
        src: PathBuf,
        /// EXACT destination (if it exists: conflict, never overwrites)
        dst: PathBuf,
        /// Symlink policy (ADR 0005)
        #[arg(long, value_enum, default_value = "preserve")]
        symlinks: SymlinksArg,
        /// Resume an interrupted copy (leaves/uses `.norte-partial`, ADR 0012)
        #[arg(long)]
        resume: bool,
    },
    /// Moves/renames, with progress and clean Ctrl-C
    Mv {
        /// Source
        src: PathBuf,
        /// Exact destination
        dst: PathBuf,
        /// Symlink policy (ADR 0005; only applies to the copy+delete path)
        #[arg(long, value_enum, default_value = "preserve")]
        symlinks: SymlinksArg,
        /// Resume an interrupted move (copy+delete path, ADR 0012)
        #[arg(long)]
        resume: bool,
    },
    /// Deletes a file or directory (recursive), with progress and clean Ctrl-C
    /// Deletes PERMANENTLY (the engine's test bench; the trash lives
    /// in the TUI — ADR 0009).
    Rm {
        /// Node to delete
        path: PathBuf,
    },
    /// Creates ONE directory (no `-p`: the parent must exist; an occupied
    /// destination = conflict) — #104
    Mkdir {
        /// Directory to create (the last segment is the new name)
        path: PathBuf,
    },
    /// Establishes a remote connection (by `connections.toml` name or
    /// `sftp://…`/`ftp://…` URL), with the interactive TOFU flow (phase 6e)
    Connect {
        /// Connection name or remote URL
        target: String,
    },
    /// JSON-RPC daemon over UDS (ADR 0011; unix only in M2)
    #[cfg(unix)]
    Daemon {
        #[command(subcommand)]
        cmd: DaemonCmd,
    },
    /// Serves MCP over stdio for agents (Claude Code, Codex…): connects to
    /// the daemon as an agent session (M3-4, ADR 0024)
    #[cfg(unix)]
    Mcp {
        #[command(subcommand)]
        cmd: McpCmd,
    },
    /// Agent governance from the human side (M3-4)
    #[cfg(unix)]
    Policy {
        #[command(subcommand)]
        cmd: PolicyCmd,
    },
    /// Undoes an agent's whole session in strict LIFO (M3-4)
    #[cfg(unix)]
    Undo {
        /// Agent session (the one from the MCP bridge's `--session`)
        session: String,
    },
    /// Runs commands of already approved+activated plugins (M4-P4)
    Plugin {
        #[command(subcommand)]
        cmd: PluginCmd,
    },
    /// Search index: `index build <path>` / `index query <path> <text>` (M4)
    Index {
        #[command(subcommand)]
        cmd: IndexCmd,
    },
    /// AI suggestions (M4, ADR 0031). Opt-in via norte.toml's `[ai]`;
    /// ALWAYS produces a REVIEWABLE plan you confirm before applying
    Ai {
        #[command(subcommand)]
        cmd: AiCmd,
    },
    /// Sweeps a directory's orphaned `.norte-partial` staging (#11, ADR
    /// 0012). NOT recursive, does not touch the user's files (it recognizes
    /// staging by its exact shape). Embedded mode only
    Gc {
        /// Directory to sweep
        path: PathBuf,
        /// Minimum age in hours (a resume IN PROGRESS must not be
        /// swept: use a generous threshold)
        #[arg(long, default_value_t = 24)]
        older_than_hours: u64,
    },
    /// Journal audit (M3-5, ADR 0025): chain + anchors + export.
    /// Requires that NOBODY has it open: not the daemon, not an embedded
    /// frontend that has already mutated something. The DB opens read-only,
    /// but whoever holds it locks it exclusively
    Audit {
        #[command(subcommand)]
        cmd: AuditCmd,
    },
    /// Opens the TERMINAL frontend (`norte-tui`) in this directory (or the
    /// one passed). The arguments travel verbatim to the binary
    /// (`norte tui --help` explains them)
    #[command(disable_help_flag = true)]
    Tui {
        /// Arguments for `norte-tui`, verbatim
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<std::ffi::OsString>,
    },
    /// Read-only diagnostics over config layers and keymaps (H2)
    Doctor {
        /// JSON output instead of human-readable text
        #[arg(long)]
        json: bool,
    },
    /// Says WHERE every file norte reads or writes lives: the config
    /// layers, and from the resolved one `norte.toml`, the keys, the
    /// connections, the secrets, the journal, the index, the logs and the
    /// daemon socket
    Paths {
        /// JSON output instead of human-readable text
        #[arg(long)]
        json: bool,
    },
    /// norte's documentation: index, one page, search, or the EFFECTIVE
    /// keys sheet. Embedded — needs no daemon (H3g)
    Help {
        /// Page to show (`--list` enumerates them). `keys` is the
        /// keyboard sheet, generated from the effective keymap
        topic: Option<String>,
        /// Enumerates the pages: id and title
        #[arg(long)]
        list: bool,
        /// Searches titles, tags, commands and body
        #[arg(long, value_name = "TEXT")]
        search: Option<String>,
        /// JSON output (for agents and goldens)
        #[arg(long)]
        json: bool,
    },
    /// Themes: imports one from VS Code into `<config>/themes/`. No engine or daemon
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
    /// Compares two trees and answers in the EXIT CODE (0 equal,
    /// 1 differ, 2 could not tell)
    Compare {
        /// Left tree
        a: PathBuf,
        /// Right tree
        b: PathBuf,
        /// One row per line, in JSON, untranslated
        #[arg(long)]
        json: bool,
        /// Comparison criteria (default: the wire's)
        #[arg(long, value_delimiter = ',')]
        criteria: Vec<String>,
        /// Maximum traversal depth
        #[arg(long)]
        max_depth: Option<u32>,
        /// mtime tolerance in milliseconds
        #[arg(long)]
        mtime_tolerance_ms: Option<u32>,
    },
    /// Syncs one tree onto another in ONE direction. Plans, shows the
    /// plan, and asks before applying. Answers in the EXIT CODE
    /// (0 there was nothing to do, 1 it was applied —or shown, with
    /// `--dry-run`—, 2 it did not happen: neither planned, nor approved,
    /// nor could be applied)
    Sync {
        /// Where it reads from
        source: PathBuf,
        /// Where it writes to
        dest: PathBuf,
        /// `update` copies what is missing or changed; `mirror` also DELETES
        /// what is extra at the destination
        #[arg(long, value_enum)]
        mode: SyncModeArg,
        /// Shows the plan and stops: applies nothing
        #[arg(long)]
        dry_run: bool,
        /// Applies without asking (the plan is still printed)
        #[arg(long)]
        yes: bool,
        /// Comparison criteria
        #[arg(long, value_delimiter = ',')]
        criteria: Vec<String>,
        /// mtime tolerance in milliseconds
        #[arg(long)]
        mtime_tolerance_ms: Option<u32>,
    },
}

/// The spelling of a sync mode as the CLI sees it. Deliberately distinct
/// from the wire's `SyncMode` (hard rule 8, implicit in the plan's spec):
/// the CLI's is presentation, the wire's is a contract, and the two need
/// not change together.
#[derive(Clone, Copy, clap::ValueEnum)]
enum SyncModeArg {
    /// Copies what is missing or changed; never deletes.
    Update,
    /// `Update` plus deleting from the destination what the source lacks.
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

/// AI subcommands (M4-A2).
#[derive(Subcommand)]
enum AiCmd {
    /// Proposes a batch rename of a dir's files from an instruction;
    /// prints the plan and asks for confirmation before applying
    Rename {
        /// Directory whose files to rename
        dir: PathBuf,
        /// Instruction in natural language (e.g. "to lowercase")
        instruction: String,
        /// Applies without asking (confirmed by default — it is reviewable)
        #[arg(long)]
        yes: bool,
    },
}

/// Search index subcommands (M4, ADR 0034).
#[derive(Subcommand)]
enum IndexCmd {
    /// (Re)builds the index of a subtree (cancelable Task)
    Build {
        /// Root to index (local path or remote URL)
        path: PathBuf,
    },
    /// Searches a root's index by text
    Query {
        /// Root whose index to query
        path: PathBuf,
        /// Free text (prefix-AND of the terms)
        text: String,
        /// Result cap
        #[arg(long, default_value_t = 50)]
        limit: u32,
    },
    /// Generates embeddings for an already indexed root (requires `[ai]` + `embed_provider`)
    Embed {
        /// Root already indexed with `index build`
        path: PathBuf,
    },
    /// Semantic search; without --root it searches every root
    Semantic {
        /// Query in natural language
        text: String,
        /// Root whose index to query (default: all)
        #[arg(long)]
        root: Option<PathBuf>,
        /// Result cap (the server trims to its own maximum)
        #[arg(long, default_value_t = 20)]
        k: u32,
    },
}

/// Audit subcommands (M3-5).
#[derive(Subcommand)]
enum AuditCmd {
    /// Verifies the hash-chain (cites the first break) and the HMAC
    /// anchors. WITHOUT anchors the verdict is FAILURE (their absence is
    /// indistinguishable from a hostile deletion) unless explicit opt-out
    Verify {
        /// Accepts a journal without an anchors file (first use)
        #[arg(long)]
        allow_no_anchors: bool,
    },
    /// Exports the journal to STDOUT
    Export {
        /// Output format
        #[arg(long, value_enum, default_value_t = AuditFormat::Jsonl)]
        format: AuditFormat,
    },
    /// Anchors the chain's current head (HMAC with a keyring key) and
    /// writes the line to STDOUT — keep it OFF this machine too: the
    /// external copy is what makes a file truncation detectable
    Anchor,
}

/// The audit export's format.
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum AuditFormat {
    /// One JSON line per entry (stable, for machines)
    Jsonl,
    /// RFC 4180 CSV with neutralized formulas (for humans)
    Csv,
}

/// Plugin subcommands.
#[derive(Subcommand)]
enum PluginCmd {
    /// Runs a plugin's command and writes its output to STDOUT
    Run {
        /// Plugin id (reverse-DNS, e.g. `org.norte.demo`)
        id: String,
        /// Command declared by the plugin
        command: String,
        /// Command argument (absent = "")
        #[arg(default_value = "")]
        arg: String,
    },
    /// Installs a plugin from a local directory (`plugin.toml` + `plugin.wasm`)
    Install {
        /// Directory with the plugin
        path: std::path::PathBuf,
        /// Replaces one already installed with the same id. WITHDRAWS its consent
        #[arg(long)]
        force: bool,
    },
    /// Uninstalls a plugin by its id. WITHDRAWS its consent
    Uninstall {
        /// Plugin id (reverse-DNS, e.g. `org.norte.demo`)
        id: String,
    },
    /// Lists installed plugins with their status (approved, activated) and capabilities
    List,
}

/// MCP subcommands.
#[cfg(unix)]
#[derive(Subcommand)]
enum McpCmd {
    /// Serves MCP over stdio until EOF (starts the daemon if needed)
    Serve {
        /// Agent session id (`[A-Za-z0-9._-]`, 1..=64)
        #[arg(long, default_value = "mcp")]
        session: String,
    },
}

/// Policy subcommands (human side).
#[cfg(unix)]
#[derive(Subcommand)]
enum PolicyCmd {
    /// Grants a pending scope request (the `request_id` is printed by
    /// the agent when it calls the `request_scope` tool)
    Grant {
        /// `request_id` returned by `request_scope`
        request_id: u64,
    },
}

/// Daemon subcommands.
#[cfg(unix)]
#[derive(Subcommand)]
enum DaemonCmd {
    /// Serves in the foreground until shutdown (request, SIGTERM/Ctrl-C or
    /// idleness)
    Run {
        /// Socket path (default: `$XDG_RUNTIME_DIR/norte/daemon.sock`)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Shuts down after N seconds without clients or tasks (0 = never)
        #[arg(long, default_value_t = 300)]
        idle_timeout: u64,
    },
    /// Asks the running daemon to shut down
    Stop {
        /// Socket path (default: same as run)
        #[arg(long)]
        socket: Option<PathBuf>,
        /// Cancels live tasks instead of waiting for them
        #[arg(long)]
        hard: bool,
        /// Handover: tells clients to COME BACK (a new daemon is coming)
        ///
        /// Without this, stopping the daemon tells them not to come back —
        /// which is correct when you stop it yourself, and the opposite of
        /// what is needed when you are replacing it. Refuses if there are
        /// live tasks.
        #[arg(long)]
        handover: bool,
    },
}

/// `cp`/`mv`'s symlink policy (maps 1:1 to the protocol's).
#[derive(Clone, Copy, Debug, clap::ValueEnum)]
enum SymlinksArg {
    /// Copies the LINK as-is (like `cp -a`)
    Preserve,
    /// Symlinks are not copied
    Skip,
    /// Copies the pointed-to CONTENT; dir-symlinks expand as real
    /// dirs (a link cycle aborts the operation)
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
    let _ = writeln!(std::io::stderr(), "warning: {phrase}");
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
        return cmd::environment::doctor_cmd(json).await;
    }
    // `paths` resolves paths and stats them, nothing more: same place for
    // the same reason. And BEFORE building an engine: the question "where
    // is my config" is asked exactly when something about it is broken.
    if let Cmd::Paths { json } = cli.cmd {
        return cmd::environment::paths_cmd(json, cli.socket.clone()).await;
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
        return Ok(cmd::environment::shell_init_cmd(shell));
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
    let mut notices = Vec::new();
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
        norte_core::team::with_index(base, &dir, &mut notices).await
    };
    if !cli.daemon {
        // Local provider, connector and — only for the two commands that
        // use them — the embeddings: what every engine carries
        // (`norte_core::team`). Loading `[ai]` resolves secrets, and an
        // `ls` never pays for that. With `--daemon` the daemon owns all
        // of this.
        let ia = norte_core::team::Ia {
            renamed: false,
            embeddings: matches!(
                cli.cmd,
                Cmd::Index {
                    cmd: IndexCmd::Embed { .. } | IndexCmd::Semantic { .. }
                }
            ),
        };
        let dir = norte_core::connect::config_dir();
        notices.extend(norte_core::team::equipar(&engine, &dir, ia).await.notices);
        // `[archive]` also in embedded mode: without this a `norte ls`
        // inside a zip used the default limits even if `norte.toml` set
        // others. Broken, here it is a warning; in the daemon, a startup
        // error.
        if let Err(e) = norte_core::archive_config::apply(&engine).await {
            notices.push(norte_core::team::Notice::ArchiveInvalid(e.to_string()));
        }
    }
    for notice in &notices {
        eprintln!("{}", cmd::daemon::warning_text(notice));
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
