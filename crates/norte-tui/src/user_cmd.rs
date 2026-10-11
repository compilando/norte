//! The TUI's half of user commands (ADR 0177): the decision is
//! `norte_frontend::user_commands`'; this only routes it.

use std::path::PathBuf;

use norte_frontend::user_commands::{self as uc, Finished, Launch, Refusal, RunIn};

use crate::app::{App, Modal};
use crate::jobs::InFlight;

/// What a spawned user-command job hands back to the loop.
#[derive(Debug)]
pub enum Outcome {
    /// A background run ended.
    Finished {
        /// What ran.
        launch: Launch,
        /// How it ended.
        finished: Finished,
    },
    /// A terminal-mode launch, its program resolved (or not) off the loop.
    Ready {
        /// What is to run.
        launch: Launch,
        /// The program on `PATH`, or why not.
        program: Result<PathBuf, Refusal>,
    },
}

/// `user:<id>` pressed or picked in the box: queues the launch, asks first
/// (`confirm = true`), or says on the bar why not — an id commands.toml does
/// not define included. The targets are the marks, or the cursor's entry
/// (never the `..` row: `selected()` is `None` over it).
pub fn request(app: &mut App, id: &str) {
    let lang = norte_i18n::active();
    let pane = app.focused();
    let targets = pane.marked_paths();
    match uc::request(&app.user_commands, id, pane.dir(), &targets) {
        Err(r) => app.message = Some(r.message(lang)),
        Ok(launch) if launch.confirm => app.modal = Some(Modal::ConfirmUserCommand { launch }),
        Ok(launch) => app.pending_user = Some(launch),
    }
}

/// Starts what `request` (or the confirm) queued. Called once per loop turn.
pub fn start_pending(app: &mut App, work: &mut InFlight) {
    let Some(launch) = app.pending_user.take() else {
        return;
    };
    if launch.run_in == RunIn::Background {
        app.message = Some(norte_i18n::ta(
            "user-cmd-started",
            &[(
                "label",
                &norte_encoding::mask_terminal_hazards(&launch.label),
            )],
        ));
    }
    work.user_commands.spawn(outcome_of(launch));
}

/// The job itself: run to the end, or resolve for the terminal panel.
pub async fn outcome_of(launch: Launch) -> Outcome {
    match launch.run_in {
        RunIn::Background => {
            let finished = uc::run_background(launch.clone()).await;
            Outcome::Finished { launch, finished }
        }
        RunIn::Terminal => {
            let probe = launch.clone();
            let program = tokio::task::spawn_blocking(move || uc::resolve(&probe))
                .await
                .unwrap_or_else(|_| {
                    Err(Refusal::Missing {
                        program: launch.program.to_string_lossy().into_owned(),
                    })
                });
            Outcome::Ready { launch, program }
        }
    }
}

/// Applies a finished job. `true` = re-list the panes.
pub fn harvest(app: &mut App, out: Outcome) -> bool {
    let lang = norte_i18n::active();
    match out {
        Outcome::Finished { launch, finished } => {
            app.message = Some(finished.message(&launch.label, lang));
            launch.refresh && finished.ran()
        }
        Outcome::Ready {
            program: Err(r), ..
        } => {
            app.message = Some(r.message(lang));
            false
        }
        Outcome::Ready {
            launch,
            program: Ok(p),
        } => {
            if let Err(msg) = crate::termpanel::start_user_command(app, &launch, p) {
                app.message = Some(msg);
            }
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::Pane;
    use norte_frontend::user_commands::UserCommands;
    use norte_proto::{Entry, EntryKind, Segment, VPath};

    fn entry(dir: &VPath, name: &str) -> Entry {
        Entry {
            attrs: std::collections::BTreeMap::new(),
            path: dir.join(Segment::new(name.as_bytes().to_vec()).expect("segment")),
            kind: EntryKind::File,
            size: Some(1),
            mtime_ms: None,
        }
    }

    /// Both panes on the real `dir`, listing `names` as files; nothing
    /// marked, the cursor on the first row.
    fn local_app(dir: &std::path::Path, names: &[&str]) -> App {
        let dir = norte_vfs::native::vpath_from_native(dir).expect("native");
        let pane = || Pane::new(dir.clone(), names.iter().map(|n| entry(&dir, n)).collect());
        App::new(pane(), pane())
    }

    const CMDS: &str = "[[command]]\nid = \"touch\"\nlabel = \"Touch\"\nargv = [\"touch\", \"%F\"]\n\
        [[command]]\nid = \"ask\"\nlabel = \"Ask\"\nargv = [\"true\", \"%f\"]\nconfirm = true\n";

    #[test]
    fn a_key_queues_a_launch_on_the_cursor_file() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.user_commands = UserCommands::parse(CMDS).unwrap();
        request(&mut app, "touch");
        let l = app.pending_user.as_ref().expect("queued");
        assert_eq!(l.args, [d.path().join("a.txt").into_os_string()]);
        assert!(app.modal.is_none());
    }

    #[test]
    fn confirm_true_opens_the_question_instead() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.user_commands = UserCommands::parse(CMDS).unwrap();
        request(&mut app, "ask");
        assert!(app.pending_user.is_none());
        assert!(matches!(app.modal, Some(Modal::ConfirmUserCommand { .. })));
    }

    /// The open item from Task 4: a well-formed `user:<id>` binding whose id
    /// commands.toml does not define loads, and says so when pressed.
    #[test]
    fn refusals_go_to_the_bar() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        request(&mut app, "nope");
        assert!(app.message.as_deref().unwrap_or("").contains("nope"));
        assert!(app.pending_user.is_none());
    }

    /// Regression pin: `marked_paths` already skips `..` (`selected()` is
    /// `None` over it), so `%F` on the parent row is "nothing selected",
    /// never a run on the parent folder.
    #[test]
    fn parent_row_is_not_a_target() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.set_parent_row(true);
        assert!(app.focused().is_parent_row(0), "the tempdir has a parent");
        app.focused_mut().set_cursor(0);
        app.user_commands = UserCommands::parse(CMDS).unwrap();
        request(&mut app, "touch");
        assert!(app.pending_user.is_none());
        assert!(app.message.is_some());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_background_run_comes_back_and_asks_for_a_refresh() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.user_commands = UserCommands::parse(
            "[[command]]\nid = \"mk\"\nlabel = \"Make\"\nargv = [\"touch\", \"made\"]\n",
        )
        .unwrap();
        request(&mut app, "mk");
        let launch = app.pending_user.take().unwrap();
        let out = outcome_of(launch).await;
        assert!(harvest(&mut app, out), "refresh = true by default");
        assert!(d.path().join("made").exists());
        assert!(app.message.as_deref().unwrap_or("").contains("Make"));
    }

    /// What the confirmation queued (`confirm_modal` sets `pending_user`)
    /// becomes a job on the loop's next turn, and the bar says it started.
    #[tokio::test]
    async fn a_queued_launch_becomes_a_job_and_says_so() {
        let d = tempfile::tempdir().unwrap();
        let mut app = local_app(d.path(), &["a.txt"]);
        app.user_commands = UserCommands::parse(CMDS).unwrap();
        request(&mut app, "ask");
        let Some(Modal::ConfirmUserCommand { launch }) = app.modal.clone() else {
            panic!("asked first");
        };
        let mut work = InFlight::default();
        app.pending_user = Some(launch);
        start_pending(&mut app, &mut work);
        assert!(app.pending_user.is_none(), "taken");
        assert_eq!(work.user_commands.len(), 1, "one job in flight");
        assert!(app.message.as_deref().unwrap_or("").contains("Ask"));
    }
}
