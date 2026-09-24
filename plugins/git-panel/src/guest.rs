//! The WASM layer: the `norte-panel` world's bindings and nothing more.
//!
//! Everything that decides anything lives in [`crate`] and is tested on the
//! host. Here only translation happens: the host's token into relative
//! reads, and the read state into styled, clickable lines.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

// The world is QUALIFIED with its package: the `wit` here is a symlink to
// the host's, whose root package is `norte:plugin`, and `norte:panel` is one
// of its dependencies. Without the prefix, wit-bindgen looks for the world
// at the root and does not find it. Same path as
// `norte:renamer/norte-renamer` in date-prefix.
wit_bindgen::generate!({
    world: "norte:panel/norte-panel",
    path: "wit",
    generate_all,
});

use exports::norte::panel::panel::{
    Frame, Guest as PanelGuest, Hit, LocationRef, PanelContext, PanelEvent, Span,
};
use norte::host::{host_config, host_log};
use norte::location::location;

/// A span with a theme ROLE: the color is chosen by the reader's theme, not
/// this plugin. This is what makes the panel look like the rest of norte
/// under any theme, light or dark.
fn role(text: &str, role: &str) -> Span {
    Span {
        text: text.to_string(),
        role: Some(role.to_string()),
        fg: None,
        bg: None,
    }
}

/// An unstyled span: the panel's default color.
fn plain(text: &str) -> Span {
    Span {
        text: text.to_string(),
        role: None,
        fg: None,
        bg: None,
    }
}

/// How many moves the configuration asks for. A value that is not a number
/// is not an error: the factory default is used, which is what the rest of
/// the host does with a badly written key.
fn moves_cap() -> usize {
    host_config::get("moves")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(crate::MOVES_DEFAULT)
}

/// A single line, for the cases where there is no repository to report on.
fn single_line(text: &str, state: Vec<u8>) -> Frame {
    Frame {
        lines: vec![vec![role(text, "muted")]],
        hits: Vec::new(),
        state,
    }
}

struct GitPanel;

impl PanelGuest for GitPanel {
    fn render(
        kind: String,
        context: PanelContext,
        location: Option<LocationRef>,
        state: Vec<u8>,
        event: PanelEvent,
    ) -> Result<Frame, String> {
        if kind != crate::PANEL_KIND {
            return Err(format!("this plugin does not paint «{kind}»"));
        }
        // The event does not change what gets painted: this panel describes
        // the repository, and the repository does not depend on where the
        // click landed. The command is logged because it is the only thing
        // a panel can receive today without the reader seeing it, and a
        // plugin that reacts silently is a plugin nobody can debug.
        if let PanelEvent::Command(cmd) = &event {
            host_log::log(&format!("git-panel: command {cmd}"));
        }

        // Without an approved location there is nothing to read, and saying
        // so is the correct response: the slot keeps painting, with a line
        // that explains why it is empty instead of staying blank.
        let Some(loc) = location else {
            return Ok(single_line("sin permiso de lectura", state));
        };
        let Ok(head) = location::read(&loc.token, b".git/HEAD") else {
            // The root the host opened has no `.git`: it is not a
            // repository, or the location is not local (sftp, s3, inside an
            // archive).
            return Ok(single_line("aquí no hay un repositorio", state));
        };

        let branch = crate::head_branch(&head);
        let reflog = location::read(&loc.token, b".git/logs/HEAD").unwrap_or_default();
        let (commit, moves) = crate::del_reflog(&reflog, moves_cap());

        let mut lines: Vec<Vec<Span>> = Vec::new();
        let mut hits: Vec<Hit> = Vec::new();

        // The branch, on the first line: it is the datum glanced at first.
        // A detached `HEAD` is said plainly, because it is a state in which
        // things get done that are later lost.
        let label = "rama ";
        match &branch {
            Some(name) => lines.push(vec![role(label, "muted"), role(name, "title")]),
            None => lines.push(vec![
                role(label, "muted"),
                role("(HEAD desprendido)", "warning"),
            ]),
        }

        if let Some(sha) = &commit {
            lines.push(vec![role("commit ", "muted"), plain(sha)]);
        } else {
            lines.push(vec![role("sin commits todavía", "muted")]);
        }

        if !moves.is_empty() {
            lines.push(Vec::new());
            lines.push(vec![role("últimos movimientos", "muted")]);
            for m in &moves {
                // The reason is trimmed to the panel's width: the guest
                // knows how many columns it has (`context.cols`), and a
                // line the host had to cut would say less than one that
                // already fits.
                let width = (context.cols as usize).saturating_sub(15).max(8);
                let reason: String = m.reason.chars().take(width).collect();
                lines.push(vec![plain(&m.to), plain(" "), role(&reason, "muted")]);
            }
        }

        // One clickable zone, and only one: open the log panel, which is
        // where what norte did with the repository can be seen. The
        // command is from the catalogue and within what a zone may name;
        // the host would refuse any other, and rightly so.
        let footer = "[registro]";
        let row_idx = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        lines.push(vec![role(footer, "muted")]);
        hits.push(Hit {
            row: row_idx,
            col: 0,
            width: u16::try_from(footer.chars().count()).unwrap_or(u16::MAX),
            command: "layout.log".to_string(),
            arg: None,
        });

        // Nothing to remember between repaints: what is shown is read whole
        // every time, and they are two small files. Returning the state
        // that came in would fake a continuity that does not exist.
        Ok(Frame {
            lines,
            hits,
            state: Vec::new(),
        })
    }
}

export!(GitPanel);
