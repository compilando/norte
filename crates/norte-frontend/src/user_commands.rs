//! The user's own commands (`commands.toml`): a program norte runs on the
//! selection, bound to a key as `user:<id>` or picked from the box.
//!
//! ```toml
//! [[command]]
//! id = "optimize-png"
//! label = "Optimize PNGs"
//! argv = ["oxipng", "-o", "4", "%F"]
//! run_in = "background"   # or "terminal"
//! cwd = "pane"            # or an absolute path
//! confirm = false
//! refresh = true
//! ```
//!
//! `argv` is never run through a shell; `%f`, `%F` and `%d` are expanded as
//! whole tokens, same contract as the openers. Only the user and system
//! layers are read, never a project's (ADR 0177).

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use norte_i18n::Lang;
use norte_proto::VPath;
use serde::Deserialize;

/// What is wrong with a `commands.toml`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UserCommandError {
    /// It does not parse, or has a key nobody reads.
    #[error("{0}")]
    Toml(String),
    /// `id` outside `[a-z0-9][a-z0-9-]{0,63}`.
    #[error("user command {id:?}: the id must be lowercase letters, digits and dashes, 64 at most")]
    BadId {
        /// The id as written.
        id: String,
    },
    /// No program to run.
    #[error("user command {id:?}: `argv` needs a program")]
    EmptyArgv {
        /// The command.
        id: String,
    },
    /// An empty `label`: the box would show a blank row.
    #[error("user command {id:?}: `label` cannot be empty")]
    EmptyLabel {
        /// The command.
        id: String,
    },
    /// `cwd` neither `"pane"` nor absolute.
    #[error("user command {id:?}: `cwd` must be \"pane\" or an absolute path")]
    BadCwd {
        /// The command.
        id: String,
    },
    /// A whole `%x` token norte does not expand (only `%f`, `%F`, `%d`).
    #[error("user command {id:?}: unknown field code {code:?} (norte expands %f, %F and %d)")]
    BadFieldCode {
        /// The command.
        id: String,
        /// The token.
        code: String,
    },
    /// The same id twice in ONE file.
    #[error("user command {id:?} is defined twice in the same file")]
    DuplicateId {
        /// The id.
        id: String,
    },
}

/// Where a command runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunIn {
    /// Detached; output to the log panel, exit in the status bar.
    #[default]
    Background,
    /// A new entry in the terminal panel (ADR 0176).
    Terminal,
}

/// The working directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cwd {
    /// The focused pane's folder.
    Pane,
    /// This absolute path.
    Fixed(PathBuf),
}

/// One validated `[[command]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserCommand {
    /// `user:<id>` in the keymap.
    pub id: String,
    /// The box's row text.
    pub label: String,
    /// What F1 on its row says.
    pub description: Option<String>,
    /// Program and argument template (`%f`, `%F`, `%d`).
    pub argv: Vec<String>,
    /// Working directory.
    pub cwd: Cwd,
    /// Background or terminal.
    pub run_in: RunIn,
    /// Ask first.
    pub confirm: bool,
    /// Re-list the panes when a background run exits.
    pub refresh: bool,
}

/// Every command norte knows, in declaration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserCommands {
    cmds: Vec<UserCommand>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    #[serde(default)]
    command: Vec<RawCommand>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCommand {
    id: String,
    label: String,
    argv: Vec<String>,
    description: Option<String>,
    cwd: Option<String>,
    run_in: Option<RawRunIn>,
    confirm: Option<bool>,
    refresh: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "lowercase")]
enum RawRunIn {
    Background,
    Terminal,
}

/// The `user:<id>` charset, `[a-z0-9][a-z0-9-]{0,63}`. SINGLE SOURCE: the
/// keymap validates `user:` bindings with it (`keymap::effective`).
#[must_use]
pub fn valid_user_id(id: &str) -> bool {
    let b = id.as_bytes();
    !b.is_empty()
        && b.len() <= 64
        && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit())
        && b.iter()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == b'-')
}

impl UserCommands {
    /// No commands (no `commands.toml` anywhere).
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parses and validates one layer's `commands.toml`.
    ///
    /// ```
    /// use norte_frontend::user_commands::UserCommands;
    ///
    /// let c = UserCommands::parse(
    ///     "[[command]]\nid = \"optimize-png\"\nlabel = \"Optimize PNGs\"\nargv = [\"oxipng\", \"%F\"]\n",
    /// )
    /// .unwrap();
    /// assert!(c.get("optimize-png").is_some());
    /// ```
    ///
    /// # Errors
    /// [`UserCommandError`] naming the command at fault.
    pub fn parse(s: &str) -> Result<Self, UserCommandError> {
        let raw: RawFile =
            toml::from_str(s).map_err(|e| UserCommandError::Toml(e.message().to_owned()))?;
        let mut cmds: Vec<UserCommand> = Vec::with_capacity(raw.command.len());
        for r in raw.command {
            let id = r.id;
            if !valid_user_id(&id) {
                return Err(UserCommandError::BadId { id });
            }
            if cmds.iter().any(|c| c.id == id) {
                return Err(UserCommandError::DuplicateId { id });
            }
            if r.label.trim().is_empty() {
                return Err(UserCommandError::EmptyLabel { id });
            }
            if r.argv.first().is_none_or(String::is_empty) {
                return Err(UserCommandError::EmptyArgv { id });
            }
            // `skip(1)`: argv[0] is the program and is never expanded, so a
            // `%x` there is just a (odd) program name. A code is `%` + ONE char.
            if let Some(code) = r.argv.iter().skip(1).find(|t| {
                t.strip_prefix('%')
                    .is_some_and(|rest| rest.chars().count() == 1)
                    && !matches!(t.as_str(), "%f" | "%F" | "%d")
            }) {
                return Err(UserCommandError::BadFieldCode {
                    id,
                    code: code.clone(),
                });
            }
            let cwd = match r.cwd.as_deref() {
                None | Some("pane") => Cwd::Pane,
                Some(p) if std::path::Path::new(p).is_absolute() => Cwd::Fixed(PathBuf::from(p)),
                Some(_) => return Err(UserCommandError::BadCwd { id }),
            };
            cmds.push(UserCommand {
                id,
                label: r.label,
                description: r.description,
                argv: r.argv,
                cwd,
                run_in: match r.run_in {
                    Some(RawRunIn::Terminal) => RunIn::Terminal,
                    _ => RunIn::Background,
                },
                confirm: r.confirm.unwrap_or(false),
                refresh: r.refresh.unwrap_or(true),
            });
        }
        Ok(Self { cmds })
    }

    /// The command with this id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&UserCommand> {
        self.cmds.iter().find(|c| c.id == id)
    }

    /// All of them, in order.
    pub fn iter(&self) -> impl Iterator<Item = &UserCommand> {
        self.cmds.iter()
    }

    /// No commands.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cmds.is_empty()
    }

    /// Lays a HIGHER layer on top: same id replaces it in place; new ids are
    /// appended.
    pub fn overlay(&mut self, higher: UserCommands) {
        for c in higher.cmds {
            match self.cmds.iter_mut().find(|m| m.id == c.id) {
                Some(slot) => *slot = c,
                None => self.cmds.push(c),
            }
        }
    }
}

impl UserCommand {
    /// Does the template need a selection (`%f`/`%F`)?
    #[must_use]
    pub fn uses_targets(&self) -> bool {
        self.argv.iter().skip(1).any(|t| t == "%f" || t == "%F")
    }
}

/// Bytes an expanded argv may take before norte refuses it. Half of macOS's
/// `ARG_MAX` (1 MiB, which includes the environment); Linux allows more.
pub const ARGV_BUDGET: usize = 512 * 1024;

/// A command ready to start: program as written (resolution is
/// [`resolve`]'s, off the async thread), arguments expanded byte-safely.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    /// The command's id.
    pub id: String,
    /// Its label, for messages.
    pub label: String,
    /// The program, as written (not yet resolved on `PATH`).
    pub program: OsString,
    /// The expanded arguments, one element per argv token or path.
    pub args: Vec<OsString>,
    /// Where it runs.
    pub cwd: PathBuf,
    /// Background or terminal.
    pub run_in: RunIn,
    /// Re-list the panes when a background run exits.
    pub refresh: bool,
    /// Ask first.
    pub confirm: bool,
    /// How many targets it runs on (0 when the template takes none).
    pub count: usize,
}

/// Why a command did not start. `key`/`args` feed Fluent, and every
/// interpolated value is untrusted text, masked by [`Refusal::args`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// No command has this id.
    Unknown {
        /// The id asked for.
        id: String,
    },
    /// The pane or a target is not a local path.
    Remote,
    /// The template needs a selection and there is none.
    NoTargets {
        /// The command's label.
        label: String,
    },
    /// The expanded argv is over [`ARGV_BUDGET`].
    TooLong {
        /// The command's label.
        label: String,
        /// How many targets.
        count: usize,
    },
    /// The program is not on `PATH`.
    Missing {
        /// The program as written.
        program: String,
    },
}

impl Refusal {
    /// Its Fluent key.
    #[must_use]
    pub fn key(&self) -> &'static str {
        match self {
            Self::Unknown { .. } => "user-cmd-unknown",
            Self::Remote => "user-cmd-remote",
            Self::NoTargets { .. } => "user-cmd-no-targets",
            Self::TooLong { .. } => "user-cmd-too-long",
            Self::Missing { .. } => "user-cmd-missing",
        }
    }

    /// Its Fluent arguments, masked for a terminal.
    #[must_use]
    pub fn args(&self) -> Vec<(&'static str, String)> {
        let m = |s: &str| norte_encoding::mask_terminal_hazards(s);
        match self {
            Self::Unknown { id } => vec![("id", m(id))],
            Self::Remote => vec![],
            Self::NoTargets { label } => vec![("label", m(label))],
            Self::TooLong { label, count } => {
                vec![("label", m(label)), ("count", count.to_string())]
            }
            Self::Missing { program } => vec![("program", m(program))],
        }
    }

    /// The status-bar line.
    #[must_use]
    pub fn message(&self, lang: Lang) -> String {
        let args = self.args();
        let refs: Vec<(&str, &str)> = args.iter().map(|(k, v)| (*k, v.as_str())).collect();
        norte_i18n::ta_in(lang, self.key(), &refs)
    }
}

/// `id` + the focused pane + its targets (marks, or the cursor) → a launch,
/// or why not. Pure: no disk.
///
/// # Errors
/// [`Refusal`] — unknown id, a remote pane, no selection for a `%f`/`%F`
/// template, or an argv over [`ARGV_BUDGET`].
pub fn request(
    cmds: &UserCommands,
    id: &str,
    dir: &VPath,
    targets: &[VPath],
) -> Result<Launch, Refusal> {
    use norte_vfs::native::vpath_to_native;
    let cmd = cmds
        .get(id)
        .ok_or_else(|| Refusal::Unknown { id: id.to_owned() })?;
    let dir_native = vpath_to_native(dir).map_err(|_| Refusal::Remote)?;
    let files: Vec<PathBuf> = targets
        .iter()
        .map(vpath_to_native)
        .collect::<Result<_, _>>()
        .map_err(|_| Refusal::Remote)?;
    if cmd.uses_targets() && files.is_empty() {
        return Err(Refusal::NoTargets {
            label: cmd.label.clone(),
        });
    }
    let refs: Vec<&Path> = files.iter().map(PathBuf::as_path).collect();
    let mut argv = crate::openers::expand_argv(&cmd.argv, &refs, &dir_native);
    if argv.iter().map(|a| a.len() + 1).sum::<usize>() > ARGV_BUDGET {
        return Err(Refusal::TooLong {
            label: cmd.label.clone(),
            count: files.len(),
        });
    }
    if argv.is_empty() {
        // `parse` refuses an empty argv; never panic if one slips through.
        return Err(Refusal::Missing {
            program: String::new(),
        });
    }
    let program = argv.remove(0);
    Ok(Launch {
        id: cmd.id.clone(),
        label: cmd.label.clone(),
        program,
        args: argv,
        cwd: match &cmd.cwd {
            Cwd::Pane => dir_native,
            Cwd::Fixed(p) => p.clone(),
        },
        run_in: cmd.run_in,
        refresh: cmd.refresh,
        confirm: cmd.confirm,
        count: if cmd.uses_targets() { files.len() } else { 0 },
    })
}

/// Finds the launch's program on `PATH`.
///
/// Blocking: stats the disk — call it from `spawn_blocking` in async code.
///
/// # Errors
/// [`Refusal::Missing`] when it is not there.
pub fn resolve(launch: &Launch) -> Result<PathBuf, Refusal> {
    resolve_in(launch, std::env::var_os("PATH").as_deref())
}

/// [`resolve`] against an explicit `PATH` value (for tests).
///
/// # Errors
/// [`Refusal::Missing`] when it is not there.
pub fn resolve_in(launch: &Launch, path_var: Option<&OsStr>) -> Result<PathBuf, Refusal> {
    crate::openers::resolve_program_in(&launch.program, path_var).ok_or_else(|| Refusal::Missing {
        program: launch.program.to_string_lossy().into_owned(),
    })
}

/// The confirm dialog's question.
#[must_use]
pub fn confirm_text(launch: &Launch, lang: Lang) -> String {
    let label = norte_encoding::mask_terminal_hazards(&launch.label);
    norte_i18n::ta_in(
        lang,
        "user-cmd-confirm",
        &[("label", &label), ("count", &launch.count.to_string())],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const OK: &str = r#"
[[command]]
id = "optimize-png"
label = "Optimize PNGs"
argv = ["oxipng", "-o", "4", "%F"]

[[command]]
id = "here"
label = "Open a terminal here"
description = "kitty in the pane's folder"
argv = ["kitty", "--directory", "%d"]
run_in = "terminal"
cwd = "/tmp"
confirm = true
refresh = false
"#;

    #[test]
    fn parses_defaults_and_overrides() {
        let c = UserCommands::parse(OK).expect("valid");
        let png = c.get("optimize-png").expect("defined");
        assert_eq!(png.label, "Optimize PNGs");
        assert_eq!(png.run_in, RunIn::Background);
        assert_eq!(png.cwd, Cwd::Pane);
        assert!(!png.confirm);
        assert!(png.refresh, "refresh defaults to true");
        assert!(png.uses_targets());
        let here = c.get("here").expect("defined");
        assert_eq!(here.run_in, RunIn::Terminal);
        assert_eq!(here.cwd, Cwd::Fixed(std::path::PathBuf::from("/tmp")));
        assert!(here.confirm && !here.refresh);
        assert_eq!(
            here.description.as_deref(),
            Some("kitty in the pane's folder")
        );
        assert!(!here.uses_targets(), "%d alone needs no selection");
    }

    #[test]
    fn id_charset() {
        for ok in ["a", "0", "optimize-png", &"a".repeat(64)] {
            assert!(valid_user_id(ok), "{ok}");
        }
        for bad in ["", "-a", "A", "a_b", "a.b", "a b", "ñ", &"a".repeat(65)] {
            assert!(!valid_user_id(bad), "{bad}");
        }
    }

    #[test]
    fn rejects_each_defect_by_name() {
        let cases: &[(&str, UserCommandError)] = &[
            (
                "[[command]]\nid = \"Bad\"\nlabel = \"x\"\nargv = [\"a\"]\n",
                UserCommandError::BadId { id: "Bad".into() },
            ),
            (
                "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = []\n",
                UserCommandError::EmptyArgv { id: "a".into() },
            ),
            (
                "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"\"]\n",
                UserCommandError::EmptyArgv { id: "a".into() },
            ),
            (
                "[[command]]\nid = \"a\"\nlabel = \"\"\nargv = [\"a\"]\n",
                UserCommandError::EmptyLabel { id: "a".into() },
            ),
            (
                "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\ncwd = \"rel/dir\"\n",
                UserCommandError::BadCwd { id: "a".into() },
            ),
            (
                "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\", \"%n\"]\n",
                UserCommandError::BadFieldCode {
                    id: "a".into(),
                    code: "%n".into(),
                },
            ),
            (
                "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\", \"%%\"]\n",
                UserCommandError::BadFieldCode {
                    id: "a".into(),
                    code: "%%".into(),
                },
            ),
            (
                "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\n[[command]]\nid = \"a\"\nlabel = \"y\"\nargv = [\"b\"]\n",
                UserCommandError::DuplicateId { id: "a".into() },
            ),
        ];
        for (src, want) in cases {
            assert_eq!(UserCommands::parse(src).unwrap_err(), *want, "{src}");
        }
        assert!(
            matches!(
                UserCommands::parse(
                    "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\nshell = true\n"
                ),
                Err(UserCommandError::Toml(_))
            ),
            "unknown keys are refused"
        );
        assert!(matches!(
            UserCommands::parse(
                "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\"]\nrun_in = \"popup\"\n"
            ),
            Err(UserCommandError::Toml(_))
        ));
    }

    #[test]
    fn a_non_ascii_field_code_is_refused_and_the_program_is_not_checked() {
        let bad = "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\", \"%é\"]\n";
        assert_eq!(
            UserCommands::parse(bad).unwrap_err(),
            UserCommandError::BadFieldCode {
                id: "a".into(),
                code: "%é".into()
            }
        );
        let prog = "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"%x\"]\n";
        assert!(UserCommands::parse(prog).is_ok(), "argv[0] is never a code");
    }

    #[test]
    fn a_literal_with_a_percent_inside_is_not_a_field_code() {
        // Only a WHOLE token `%x` is a code (openers.rs contract); `--q=%f`
        // and `100%` are literals and stay legal.
        let src = "[[command]]\nid = \"a\"\nlabel = \"x\"\nargv = [\"a\", \"--q=%f\", \"100%\"]\n";
        assert!(UserCommands::parse(src).is_ok());
    }

    #[test]
    fn a_higher_layer_overrides_by_id() {
        let mut low = UserCommands::parse(OK).unwrap();
        let high = UserCommands::parse(
            "[[command]]\nid = \"here\"\nlabel = \"Mine\"\nargv = [\"foot\"]\n",
        )
        .unwrap();
        low.overlay(high);
        assert_eq!(low.get("here").unwrap().label, "Mine");
        assert!(low.get("optimize-png").is_some(), "the rest survives");
        assert_eq!(low.iter().count(), 2);
    }

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("vpath")
    }
    fn cmds(src: &str) -> UserCommands {
        UserCommands::parse(src).expect("valid")
    }
    const PNG: &str = "[[command]]\nid = \"png\"\nlabel = \"Optimize\"\nargv = [\"oxipng\", \"-o\", \"4\", \"%F\"]\n";

    #[test]
    fn every_field_code_expands_as_whole_elements() {
        let c = cmds(
            "[[command]]\nid = \"x\"\nlabel = \"X\"\nargv = [\"p\", \"%f\", \"--\", \"%F\", \"%d\"]\n",
        );
        let l = request(
            &c,
            "x",
            &vp("file:///d"),
            &[vp("file:///d/a"), vp("file:///d/b")],
        )
        .unwrap();
        assert_eq!(l.program, std::ffi::OsString::from("p"));
        let args: Vec<_> = l
            .args
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        assert_eq!(args, ["/d/a", "--", "/d/a", "/d/b", "/d"]);
        assert_eq!(l.cwd, std::path::PathBuf::from("/d"));
        assert_eq!(l.count, 2);
        assert!(l.refresh && !l.confirm);
    }

    #[test]
    fn fixed_cwd_wins_but_percent_d_is_still_the_pane() {
        let c = cmds(
            "[[command]]\nid = \"x\"\nlabel = \"X\"\nargv = [\"p\", \"%d\"]\ncwd = \"/srv\"\n",
        );
        let l = request(&c, "x", &vp("file:///d"), &[]).unwrap();
        assert_eq!(l.cwd, std::path::PathBuf::from("/srv"));
        assert_eq!(l.args, [std::ffi::OsString::from("/d")]);
        assert_eq!(l.count, 0, "%d-only runs with no selection");
    }

    #[test]
    fn refusals() {
        let c = cmds(PNG);
        assert_eq!(
            request(&c, "nope", &vp("file:///d"), &[]),
            Err(Refusal::Unknown { id: "nope".into() })
        );
        assert_eq!(
            request(&c, "png", &vp("sftp://h/d"), &[vp("sftp://h/d/a")]),
            Err(Refusal::Remote)
        );
        assert_eq!(
            request(&c, "png", &vp("file:///d"), &[]),
            Err(Refusal::NoTargets {
                label: "Optimize".into()
            })
        );
    }

    #[test]
    fn too_many_targets_are_refused_with_the_count() {
        let c = cmds(PNG);
        let name = "n".repeat(200);
        let many: Vec<VPath> = (0..4000)
            .map(|i| vp(&format!("file:///d/{name}{i}")))
            .collect();
        assert_eq!(
            request(&c, "png", &vp("file:///d"), &many),
            Err(Refusal::TooLong {
                label: "Optimize".into(),
                count: 4000
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn hostile_names_are_absolute_whole_elements() {
        use std::os::unix::ffi::OsStrExt;
        let c = cmds(PNG);
        let dir = vp("file:///d");
        let dash = norte_vfs::native::vpath_from_native(std::path::Path::new("/d/-rf")).unwrap();
        let bytes = norte_vfs::native::vpath_from_native(std::path::Path::new(
            std::ffi::OsStr::from_bytes(b"/d/caf\xe9 name"),
        ))
        .unwrap();
        let l = request(&c, "png", &dir, &[dash, bytes]).unwrap();
        assert_eq!(l.args[2].as_bytes(), b"/d/-rf", "a path, never a flag");
        assert_eq!(
            l.args[3].as_bytes(),
            b"/d/caf\xe9 name",
            "bytes intact, spaces kept in ONE element"
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_finds_on_path_or_says_missing() {
        let c = cmds("[[command]]\nid = \"x\"\nlabel = \"X\"\nargv = [\"sh\"]\n");
        let l = request(&c, "x", &vp("file:///"), &[]).unwrap();
        let p = resolve_in(&l, Some(std::ffi::OsStr::new("/bin:/usr/bin"))).expect("sh exists");
        assert!(p.is_absolute());
        assert_eq!(
            resolve_in(&l, Some(std::ffi::OsStr::new("/nonexistent"))),
            Err(Refusal::Missing {
                program: "sh".into()
            })
        );
    }

    #[test]
    fn every_message_key_exists_in_both_languages() {
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let ids = norte_i18n::message_ids(lang);
            for k in [
                "user-cmd-unknown",
                "user-cmd-remote",
                "user-cmd-no-targets",
                "user-cmd-too-long",
                "user-cmd-missing",
                "user-cmd-confirm",
                "user-cmd-confirm-title",
                "user-cmd-started",
                "user-cmd-ok",
                "user-cmd-failed",
                "user-cmd-signalled",
                "user-cmd-not-started",
                "user-cmd-no-description",
                "msg-user-cmd-undefined",
                "cmd-ns-user",
            ] {
                assert!(ids.iter().any(|i| i == k), "{k} missing in {lang:?}");
            }
        }
    }
}
