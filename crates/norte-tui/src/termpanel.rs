//! The terminal pane (#362): a shell INSIDE a layout slot.
//!
//! This is what Krusader has and norte did not. What already existed is
//! different, and in one way better: `app.toggle-panels` hands the ENTIRE
//! terminal over to a live subshell (ADR 0084), and `app.terminal` launches a
//! separate shell. What was missing was seeing it AT THE SAME TIME as the
//! listings.
//!
//! # The split
//!
//! The emulation — bytes into a grid of cells — and the pty live in
//! `norte-term`: the first always, the second behind its feature. What stays
//! here is only the PAINTING with `ratatui`, the one thing it does not share
//! with the window. The day the window paints its own pane it will use the
//! same shell and the same grid, so the two frontends show the same thing by
//! construction and not because someone compared two emulators.
//!
//! # Who owns the keyboard
//!
//! This is the only pane that consumes BYTES and not catalogue commands, so
//! while it holds the keys it also keeps the chords that would otherwise
//! belong to norte. The way out is ONE loose chord — the same
//! `layout.terminal` that opened it — and [`crate::keys`] recognizes it
//! before forwarding anything. If the preset bound it to a sequence,
//! `Effective::lone_chord` returns `None` and the pane does NOT take the
//! keys: a pane you can only look at is better than one you cannot leave.
//!
//! # What this pane does NOT do yet
//!
//! It does not install the prompt hook, so the listing does not follow the
//! shell nor the shell the listing: that is what #142's subshell is for,
//! which does install it. A pane that typed `cd` into the reader's shell has
//! #363's problems — and would have them somewhere the reader watches it
//! happen — so that waits until that hole is closed.

use norte_term::{ColorTerm, Screen, Style};

/// A key as the bytes a shell expects; documented in `subshell`, which
/// re-exports it. It lives here because the terminal pane needs it on every
/// platform and the subshell exists only on unix (ADR 0084).
#[must_use]
pub fn key_to_bytes(k: &crossterm::event::KeyEvent) -> Option<Vec<u8>> {
    use norte_frontend::keymap::{Chord, KeyCode, Mods};
    // **The TABLE is shared** (`norte_frontend::subshell::chord_a_bytes`),
    // and all that is left here is translating the crossterm event into
    // the canonical chord it understands. It was written twice since the
    // terminal panel (#362) also needed it in the window, and two tables
    // are two places where `F10` stops getting you out of an `htop`.
    //
    // `BackTab` is the one thing the canonical chord does not name: for
    // the keymap it is shift+tab, which is exactly what is built here.
    let (mods, code) = if k.code == crossterm::event::KeyCode::BackTab {
        (
            Mods {
                shift: true,
                ..Mods::default()
            },
            KeyCode::Tab,
        )
    } else {
        crate::keymap::chord_from_crossterm(k.modifiers, k.code)?.parts()
    };
    norte_frontend::subshell::chord_a_bytes(Chord::new(mods, code))
}

/// The pane's shell, with its grid. It is `norte-term`'s.
pub use norte_term::pty::Shell as TermPanel;

/// `norte-term`'s shell as the shared model's
/// [`TerminalShell`](norte_frontend::terminals::TerminalShell): a newtype
/// because neither the trait nor the type is this crate's.
pub struct Pty(pub TermPanel);

impl norte_frontend::terminals::TerminalShell for Pty {
    fn pump(&mut self) -> bool {
        self.0.pump()
    }
    fn resize(&mut self, size: (u16, u16)) {
        self.0.resize(size);
    }
    fn take_title(&mut self) -> Option<String> {
        self.0.take_title()
    }
    fn exit_code(&mut self) -> Option<i32> {
        self.0.exit_code()
    }
}

/// The panel's shells (spec 2026-10-09): the same model and rules as the
/// window's.
pub type Shells = norte_frontend::terminals::Terminals<Pty>;

/// Kills and reaps shells off the loop when there is a runtime: each
/// `Drop` is a kill plus a blocking wait, and a shell ignoring the hang-up
/// would freeze the screen for it. Without a runtime (a unit test) they die
/// here.
pub fn bury(gone: Vec<norte_frontend::terminals::Instance<Pty>>) {
    if gone.is_empty() {
        return;
    }
    match tokio::runtime::Handle::try_current() {
        Ok(h) => {
            h.spawn_blocking(move || drop(gone));
        }
        Err(_) => drop(gone),
    }
}

/// The kind's id, which is also its command's suffix.
pub const KIND: &str = "terminal";

/// The command that opens the pane, gives it the keyboard, and takes it away.
///
/// It is the SAME one that exits, and that is why it lives here instead of
/// being hand-written in the two places that look it up in the keymap: the
/// chord that runs it is the only one the pane does not forward to the shell.
pub const COMMAND: &str = "layout.terminal";

/// How a pane's shell is started, with what norte decides.
///
/// The program and the environment are set HERE and not in `norte-term`:
/// resolving the reader's shell and the `NORTE_LEVEL` contract are norte's
/// rules, not an emulator's.
///
/// # Errors
/// Whatever fails opening the pty or launching the shell.
pub fn open(
    dir: &std::path::Path,
    size: (u16, u16),
    profile: &norte_frontend::shell_profiles::ShellProfile,
) -> std::io::Result<TermPanel> {
    // Absolute by construction (`terminal.toml` refuses anything else), and
    // checked again here because the spawn is where a relative one would be
    // looked up from the directory being browsed (#302).
    if !profile.program.is_absolute() {
        return Err(std::io::Error::other("the shell program is not absolute"));
    }
    norte_term::pty::Shell::open(
        &norte_term::pty::Startup {
            program: &profile.program,
            args: &profile.args,
            dir,
            tam: size,
            // The child knows it is INSIDE norte, just like the subshell and
            // a suspension do: the same `NORTE_LEVEL` contract, and the
            // reader's prompt reads it to say so.
            env: &[(
                norte_frontend::shell::LEVEL_VAR.into(),
                norte_frontend::shell::next_norte_level().into(),
            )],
        },
        // The same table the subshell answers with: a terminal query gets
        // the same answer no matter where it comes from.
        norte_frontend::subshell::terminal_reply,
    )
}

/// Starts `profile`'s shell in the focused listing's directory and puts it
/// in front. `Err` is the message to show; nothing is added then.
///
/// # Errors
/// The focused pane is not local, or the shell did not start.
pub fn start_instance(
    app: &mut crate::app::App,
    profile: &norte_frontend::shell_profiles::ShellProfile,
) -> Result<(), String> {
    // A local directory, with the same gate and phrase as `app.terminal`.
    let dir = crate::gestures::shell_cwd(app)?;
    // The real size is set by the paint as soon as it knows which rectangle
    // it got; this one lasts as long as the first turn takes.
    let t = open(&dir, (80, 24), profile).map_err(|e| e.to_string())?;
    // Not journalled: a shell the reader opens is the reader acting with
    // their own permissions (ADR 0084). Logged because starting one is the
    // most privileged thing a frontend does — the shell profile's NAME only:
    // its args may carry a token.
    tracing::info!(
        shell_profile = %profile.name,
        "TUI opened a shell in a terminal panel (not journalled: no actor, no reversal)"
    );
    let wake = std::sync::Arc::clone(&app.term_wake);
    t.set_waker(std::sync::Arc::new(move || wake.notify_one()));
    app.terminals
        .push(profile.name.clone(), profile.icon, profile.color, Pty(t));
    Ok(())
}

/// What a row of the terminal picker does.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TermChoice {
    /// Start this shell profile.
    Profile(String),
    /// Set (or clear) the icon of the one in front.
    Icon(Option<norte_frontend::terminals::TerminalIcon>),
    /// Set (or clear) its colour.
    Color(Option<norte_frontend::terminals::AnsiColor>),
}

/// The small list `terminal.new-profile` and `terminal.decorate` open.
#[derive(Debug, Clone)]
pub struct TermPicker {
    /// Already translated.
    pub title: String,
    /// Label and what choosing it does.
    pub rows: Vec<(String, TermChoice)>,
    /// The highlighted row.
    pub cursor: usize,
}

/// The text field `terminal.rename` opens.
#[derive(Debug, Clone)]
pub struct TermRename {
    /// The instance it names.
    pub id: norte_frontend::terminals::InstanceId,
    /// What has been typed.
    pub text: String,
}

/// The list's labels, in list order: position, unseen dot, title, and the
/// code of a failed one.
#[must_use]
pub fn strip_labels(t: &Shells) -> Vec<(norte_frontend::terminals::InstanceId, String)> {
    t.iter()
        .enumerate()
        .map(|(n, i)| {
            let title = t.display_title(i.id).unwrap_or_default();
            let dot = if i.unseen { "● " } else { "" };
            let code = i.exited.map(|c| format!(" ({c})")).unwrap_or_default();
            (i.id, format!("{} {dot}{title}{code}", n + 1))
        })
        .collect()
}

fn not_here(app: &mut crate::app::App) {
    app.message = Some(norte_i18n::t("cmd-not-here"));
}

/// The panel's own key pressed INSIDE it. With a live shell it is the way
/// out, as always. With none — a panel the session restored (it saves the
/// slot, not the shell), or one whose shells all ended — it starts one:
/// leaving a panel that says "no shell" left the reader to guess that the
/// same key, pressed again from outside, was the way to get one.
pub fn on_door(app: &mut crate::app::App, cfg: &crate::config::LoadedConfig) {
    let live = app.terminals.iter().any(|i| i.exited.is_none());
    if live || app.key_owner() != crate::app::KeyOwner::Terminal {
        app.toggle_terminal();
    } else {
        cmd_new(app, cfg, None);
    }
}

/// `terminal.new`: another shell, from `profile` (`None` = the default).
pub fn cmd_new(
    app: &mut crate::app::App,
    cfg: &crate::config::LoadedConfig,
    profile: Option<&str>,
) {
    if app.terminal_slot().is_none() {
        return not_here(app);
    }
    let profiles = &cfg.shell_profiles;
    let Some(p) = profile
        .map_or(Some(profiles.default_profile()), |n| profiles.get(n))
        .cloned()
    else {
        return not_here(app);
    };
    if let Err(msg) = start_instance(app, &p) {
        app.message = Some(msg);
    }
}

/// `terminal.close`: the one in front, killing its shell.
pub fn cmd_close(app: &mut crate::app::App) {
    let Some(id) = app.terminals.active_id() else {
        return not_here(app);
    };
    if let Some(gone) = app.terminals.close(id) {
        bury(vec![gone]);
    }
    if app.terminals.is_empty() && app.key_owner() == crate::app::KeyOwner::Terminal {
        app.release_keyboard();
    }
}

/// `terminal.next` / `terminal.prev`.
pub fn cmd_step(app: &mut crate::app::App, forward: bool) {
    if app.terminals.is_empty() {
        return not_here(app);
    }
    if forward {
        app.terminals.next();
    } else {
        app.terminals.prev();
    }
}

/// `terminal.new-profile`: the shell profiles, default first.
pub fn cmd_pick_profile(app: &mut crate::app::App, cfg: &crate::config::LoadedConfig) {
    if app.terminal_slot().is_none() {
        return not_here(app);
    }
    let p = &cfg.shell_profiles;
    let default = &p.default_profile().name;
    let rows = std::iter::once(default.clone())
        .chain(p.iter().map(|s| s.name.clone()).filter(|n| n != default))
        .map(|n| {
            // A name the reader wrote: painted the shared way.
            let label = norte_frontend::display_name(n.as_bytes()).0;
            (label, TermChoice::Profile(n))
        })
        .collect();
    app.term_picker = Some(TermPicker {
        title: norte_i18n::t("terminal-shell-profiles"),
        rows,
        cursor: 0,
    });
}

/// `terminal.decorate`: icons and colours for the one in front.
pub fn cmd_decorate(app: &mut crate::app::App) {
    use norte_frontend::terminals::{AnsiColor, TerminalIcon};
    if app.terminals.active_id().is_none() {
        return not_here(app);
    }
    let mut rows = vec![(norte_i18n::t("terminal-icon-none"), TermChoice::Icon(None))];
    rows.extend(
        TerminalIcon::ALL
            .iter()
            .map(|i| (i.as_str().to_owned(), TermChoice::Icon(Some(*i)))),
    );
    rows.push((
        norte_i18n::t("terminal-color-none"),
        TermChoice::Color(None),
    ));
    rows.extend(
        (1..=6)
            .filter_map(AnsiColor::new)
            .map(|c| (format!("■ {}", c.index()), TermChoice::Color(Some(c)))),
    );
    app.term_picker = Some(TermPicker {
        title: norte_i18n::t("terminal-decorate-title"),
        rows,
        cursor: 0,
    });
}

/// A picker row was chosen.
pub fn apply_choice(app: &mut crate::app::App, cfg: &crate::config::LoadedConfig, c: TermChoice) {
    let front = app.terminals.active();
    let (id, icon, color) = (
        front.map(|i| i.id),
        front.and_then(|i| i.icon),
        front.and_then(|i| i.color),
    );
    match (c, id) {
        (TermChoice::Profile(n), _) => cmd_new(app, cfg, Some(&n)),
        // One attribute at a time: choosing a colour keeps the icon.
        (TermChoice::Icon(i), Some(id)) => app.terminals.decorate(id, i, color),
        (TermChoice::Color(c), Some(id)) => app.terminals.decorate(id, icon, c),
        _ => not_here(app),
    }
}

/// The keys while the terminal picker or rename field is open. `true` if
/// one of them was open and took the key.
pub fn on_overlay_key(
    app: &mut crate::app::App,
    cfg: &crate::config::LoadedConfig,
    key: &crossterm::event::KeyEvent,
) -> bool {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if key.kind != KeyEventKind::Press {
        return app.term_picker.is_some() || app.term_rename.is_some();
    }
    if let Some(r) = app.term_rename.as_mut() {
        match key.code {
            KeyCode::Esc => app.term_rename = None,
            KeyCode::Enter => {
                if let Some(r) = app.term_rename.take() {
                    // The model cleans and caps it.
                    app.terminals.rename(r.id, &r.text);
                }
            }
            KeyCode::Backspace => {
                r.text.pop();
            }
            // Capped while typing; the model caps again.
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
                    && r.text.chars().count() < 128 =>
            {
                r.text.push(c);
            }
            _ => {}
        }
        return true;
    }
    let Some(p) = app.term_picker.as_mut() else {
        return false;
    };
    match key.code {
        KeyCode::Esc => app.term_picker = None,
        KeyCode::Up => p.cursor = p.cursor.saturating_sub(1),
        KeyCode::Down => p.cursor = (p.cursor + 1).min(p.rows.len().saturating_sub(1)),
        KeyCode::Enter => {
            let choice = p.rows.get(p.cursor).map(|(_, c)| c.clone());
            app.term_picker = None;
            if let Some(c) = choice {
                apply_choice(app, cfg, c);
            }
        }
        _ => {}
    }
    true
}

/// `terminal.rename`: a field prefilled with the current name.
pub fn cmd_rename(app: &mut crate::app::App) {
    let Some(i) = app.terminals.active() else {
        return not_here(app);
    };
    app.term_rename = Some(TermRename {
        id: i.id,
        text: i.name.clone().unwrap_or_default(),
    });
}

/// The grid's rows as `ratatui` spans.
///
/// The chunking — where a row is cut — is done by `norte-term`, because it is
/// the same decision for both frontends and is made once. Here each segment
/// is only translated into this toolkit's style.
///
/// The content is FOREIGN and even so nothing is masked: what comes out of
/// the grid no longer carries any control byte, and that is guaranteed by the
/// grid, not by this code.
#[must_use]
pub fn rows<'a>(p: &Screen) -> Vec<ratatui::text::Line<'a>> {
    use ratatui::text::{Line, Span};
    let (_, height) = p.size();
    (0..height)
        .map(|row| {
            Line::from(
                p.row_tramos(row)
                    .into_iter()
                    .map(|(text, style)| Span::styled(text, style_of(style)))
                    .collect::<Vec<Span<'a>>>(),
            )
        })
        .collect()
}

/// A terminal [`Style`] translated into `ratatui`'s.
///
/// An INDEXED color passes through as-is (`Color::Indexed`): on a terminal it
/// is resolved by whatever palette the reader has set in their emulator,
/// exactly what would happen if the program ran outside norte.
///
/// **That is why norte's theme has no place here, not even as an argument.**
/// This is ANOTHER program's content, not our chrome, and a theme that
/// changed an `ls --color`'s colors would be lying about what that program
/// said. Ours is the frame, and the frame is painted by whoever draws it.
fn style_of(e: Style) -> ratatui::style::Style {
    use ratatui::style::{Modifier, Style};
    let mut s = Style::default();
    if let Some(c) = color_of(e.fg) {
        s = s.fg(c);
    }
    if let Some(c) = color_of(e.bg) {
        s = s.bg(c);
    }
    let mut m = Modifier::empty();
    if e.bold {
        m |= Modifier::BOLD;
    }
    if e.tenue {
        m |= Modifier::DIM;
    }
    if e.italic {
        m |= Modifier::ITALIC;
    }
    if e.underlined {
        m |= Modifier::UNDERLINED;
    }
    if e.inverse {
        m |= Modifier::REVERSED;
    }
    if e.strikethrough {
        m |= Modifier::CROSSED_OUT;
    }
    s.add_modifier(m)
}

fn color_of(c: ColorTerm) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    match c {
        ColorTerm::Default => None,
        ColorTerm::Indexed(n) => Some(Color::Indexed(n)),
        ColorTerm::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Where the cursor goes within the content area, if it must be painted.
///
/// Returns `None` when the shell hid it (`CSI ?25l`, which any full-screen
/// program does) or when the pane does not have the keys: a cursor blinking
/// in a pane that does not hold them says the keyboard is there, and it is
/// not.
#[must_use]
pub fn cursor_en(
    p: &Screen,
    area: ratatui::layout::Rect,
    has_keyboard: bool,
) -> Option<(u16, u16)> {
    if !has_keyboard || !p.cursor_visible() {
        return None;
    }
    let (row, col) = p.cursor();
    let (width, height) = p.size();
    // The column can equal the width — the "pending wrap" state — and there
    // the cursor is painted in the last cell: it is where a real terminal
    // leaves it.
    let col = col.min(width.saturating_sub(1));
    (row < height).then(|| (area.x + col, area.y + row))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A terminal style reaches `ratatui` with its attributes intact, and an
    /// index stays an index: resolving it here would take away the reader's
    /// emulator's own palette.
    #[test]
    fn a_terminal_style_crosses_over_whole() {
        let e = Style {
            fg: ColorTerm::Indexed(4),
            bg: ColorTerm::Rgb(1, 2, 3),
            bold: true,
            underlined: true,
            ..Style::default()
        };
        let s = style_of(e);
        assert_eq!(s.fg, Some(ratatui::style::Color::Indexed(4)));
        assert_eq!(s.bg, Some(ratatui::style::Color::Rgb(1, 2, 3)));
        assert!(s.add_modifier.contains(ratatui::style::Modifier::BOLD));
        assert!(
            s.add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }

    /// Cells in a row with the same style are ONE span: eighty spans per row
    /// is what makes a `make` in the pane felt across the rest.
    #[test]
    fn equal_cells_group_into_one_span() {
        let mut p = Screen::new(10, 1);
        p.alimentar(b"aaa\x1b[31mbbb");
        let rows = rows(&p);
        let spans = &rows[0].spans;
        // Three, not two: after `bbb` four cells are left unwritten, and
        // those carry the DEFAULT style, not red. Grouping them with the red
        // one would paint the rest of the line's background with the last
        // command's color, which is the classic bug of a hand-rolled
        // emulator.
        assert_eq!(spans.len(), 3, "two styles and the padding: {spans:?}");
        assert_eq!(spans[0].content, "aaa");
        assert_eq!(spans[1].content, "bbb");
        assert_eq!(spans[2].content, "    ");
        assert_eq!(spans[2].style, ratatui::style::Style::default());
    }

    /// With no keyboard, no cursor is painted: that would say the keyboard is
    /// here.
    #[test]
    fn the_cursor_is_only_painted_with_the_keyboard_inside() {
        let p = Screen::new(10, 3);
        let area = ratatui::layout::Rect::new(5, 2, 10, 3);
        assert_eq!(cursor_en(&p, area, false), None);
        assert_eq!(cursor_en(&p, area, true), Some((5, 2)));
    }

    /// And not either when the shell hides it, which any full-screen program
    /// does while it paints.
    #[test]
    fn a_hidden_cursor_is_not_painted() {
        let mut p = Screen::new(10, 3);
        p.alimentar(b"\x1b[?25l");
        let area = ratatui::layout::Rect::new(0, 0, 10, 3);
        assert_eq!(cursor_en(&p, area, true), None);
    }
}
