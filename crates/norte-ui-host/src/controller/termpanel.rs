//! The terminal panel in the window (#362, bridge 95).
//!
//! Two halves. The SHELL and the grid belong to `norte-term`, the same ones
//! the terminal uses: the same pty, the same reader thread and the same
//! emulation, so the two frontends show the same thing by construction and
//! not because someone compares two emulators. And the TRANSLATION of that
//! grid into what crosses the bridge, which is the only thing of its own
//! here.
//!
//! # What is NOT done in the translation, and it is the decision
//!
//! **An indexed color is not resolved.** The shell says "color 4"; which blue
//! that is, is decided by the palette of whoever paints it. If it were
//! resolved here to an `#rrggbb`, the panel would stop obeying the reader's
//! theme and there would be no way to fix it from the theme. That is why
//! [`TerminalColorView`] keeps the two cases distinct.
//!
//! **Nothing is masked.** What comes out of the grid can no longer carry a
//! control byte: the parser eats the escapes and drops the C0 codes that do
//! not move the cursor. Masking of hostile names exists because a name
//! arrives raw; this does not arrive raw, it arrives parsed.

use std::sync::Arc;

use norte_term::{ColorTerm, Estilo, Pantalla};
use tokio::sync::mpsc;

use crate::backend::HostBackend;
use crate::bridge::BridgeEnvelope;
use crate::dto::{TerminalColorView, TerminalSlotView, TerminalSpanView, UiUpdate};

use super::{ActionAck, Estado, Mensaje};

/// The panel's kind, which is also the suffix of its command.
pub(super) const KIND: &str = "terminal";

/// The command that opens the panel and the one that takes it out: it is the
/// SAME one.
pub(super) const COMANDO: &str = "layout.terminal";

impl Estado {
    /// Opens the terminal panel, or brings it to the front.
    ///
    /// **It never closes it**, unlike `alternar_hueco`: inside there is a
    /// shell of the reader's with whatever it had half-done, and closing it
    /// kills it. Closing is `layout.close-slot`, named for what it does.
    pub(super) fn abrir_panel_de_terminal(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let Some(id) = self.hueco_de_kind(KIND) {
            // Behind another tab: it is brought to the front.
            let behind = self
                .arbol
                .tabs_of(id)
                .is_some_and(|(t, a)| t.get(a) != Some(&id));
            if behind {
                return self.elegir_pestana(id.0, backend, mailbox);
            }
            // Already in front, and here is the DOOR, in both directions. The
            // key is one, so it has to take you in and bring you back:
            //
            // - with focus elsewhere, it is given to it (and without this,
            //   opening with the key never focused the panel:
            //   `abrir_hueco_de_kind` leaves focus on the remembered listing,
            //   so it was only entered with the mouse or with the ring);
            // - with focus INSIDE, it is returned to the listing. Without
            //   this the documented exit did nothing and the panel was a
            //   mousetrap: in there every key belongs to the shell, including
            //   the ring's.
            //
            // What it does NOT do, and that is the difference with
            // `alternar_hueco`: close. There is a shell of the reader's in
            // there.
            let inside = self
                .roles
                .get(norte_frontend::layout::RoleId::Active)
                .is_some_and(|a| a == id);
            let target = if inside {
                norte_frontend::layout::SlotId(self.activo())
            } else {
                id
            };
            self.roles
                .set(norte_frontend::layout::RoleId::Active, target);
            self.reconcilia_roles();
            // And if the slot exists WITHOUT a shell, it is started here. It
            // is the case of a restored session — the tree is saved, the
            // shell is not — and of a startup that failed: without this the
            // panel stayed dead for the rest of the window's life, because
            // this branch returned before checking.
            if !inside && self.terminal.is_none() {
                self.arrancar_si_falta(mailbox);
            }
            let snap = self.snapshot();
            let envelope = self.sobre(UiUpdate::Snapshot(Box::new(snap)));
            return (self.aplicada(), vec![envelope]);
        }
        // A shell sits in a filesystem directory: over an `sftp://` there is
        // nowhere to sit it, and opening it in `$HOME` without saying
        // anything would be opening it somewhere else. It is the same gate as
        // `app.terminal`, with the same reason and the same phrase.
        let dir = self.hueco().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            let outgoing = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                outgoing,
            );
        }
        let (ack, mut updates) = self.abrir_hueco_de_kind(KIND, backend, mailbox);
        // Focus goes to the panel: opening it and not being able to type
        // inside without hunting for the mouse is not opening it.
        // `abrir_hueco_de_kind` leaves it on the remembered listing, so it is
        // set here.
        if let Some(id) = self.hueco_de_kind(KIND) {
            self.roles.set(norte_frontend::layout::RoleId::Active, id);
            self.reconcilia_roles();
        }
        updates.extend(self.arrancar_si_falta(mailbox));
        (ack, updates)
    }

    /// Starts the shell if the slot exists and does not have one, and
    /// republishes.
    ///
    /// Called from both paths — opening the slot and returning to it —
    /// because the second one is the restored-session path: the tree is
    /// saved and the shell is not, so on starting the window there is a slot
    /// with no shell.
    fn arrancar_si_falta(
        &mut self,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if self.terminal.is_some() || self.hueco_de_kind(KIND).is_none() {
            return Vec::new();
        }
        let dir = self.hueco().pane.dir().clone();
        match arrancar(&dir) {
            Ok(shell) => {
                // A record is left, as the terminal does and with the same
                // "does not go to the journal" note: a shell the reader opens
                // is the reader acting with their own permissions, not a
                // mutation by norte. But starting a shell is the most
                // privileged thing a frontend does, and the log panel is now
                // a surface that is also watched here.
                tracing::info!(
                    "the window opened a shell in a terminal panel \
                     (does not go to the journal: no actor and no undo)"
                );
                self.terminal = Some(shell);
                self.sondear_terminal(mailbox);
            }
            Err(e) => {
                // And it is SAID, not just to the log: the slot stays, so
                // without a message the reader sees an empty panel with no
                // idea why.
                tracing::warn!(error = %e, "could not open the panel's shell");
                return self.decir("host-shell-failed");
            }
        }
        self.republicar_terminal()
    }

    /// Schedules the panel's next tick, if there is still a panel.
    ///
    /// It rearms only while the slot stays in the tree and turns off when it
    /// closes — same mechanism as the log panel's polling, and for the same
    /// reason: a 30 Hz timer that outlived the panel would keep waking the
    /// actor to paint nothing.
    fn sondear_terminal(&self, mailbox: &mpsc::Sender<Mensaje>) {
        if self.hueco_de_kind(KIND).is_none() {
            return;
        }
        let (mailbox, epoch) = (mailbox.clone(), self.terminal_epoca);
        tokio::spawn(async move {
            tokio::time::sleep(super::TERMINAL_TIC).await;
            let _ = mailbox.send(Mensaje::TerminalTic(epoch)).await;
        });
    }

    /// Flushes what the shell wrote and republishes if something changed.
    ///
    /// A tick with no bytes produces no patch: a quiet shell does not wake
    /// the renderer thirty times a second.
    pub(super) fn terminal_tic(
        &mut self,
        epoch: u64,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoch != self.terminal_epoca {
            // From an earlier opening: let it die without rearming.
            return Vec::new();
        }
        // If the slot is no longer there, the shell goes with it and the tick
        // does not rearm.
        if self.hueco_de_kind(KIND).is_none() {
            self.soltar_terminal();
            return Vec::new();
        }
        self.sondear_terminal(mailbox);
        // The slot's size, BEFORE pumping: the pty has to know it or a
        // full-screen program paints for a width that is not its own and
        // line wrapping comes out wrong. It started at 80x24 and nobody ever
        // resized it.
        //
        // From the LAYOUT, same as how the docked viewer gets its height:
        // minus the frame, which the renderer paints. `redimensionar` does
        // nothing if it did not change, so asking on every tick is free.
        let size = self.tamano_del_terminal();
        let Some(t) = self.terminal.as_mut() else {
            return Vec::new();
        };
        if let Some(size) = size {
            t.redimensionar(size);
        }
        let changed = t.bombear();
        // If the shell left, the slot SAYS so instead of showing the last
        // screen of a process that no longer exists. The slot stays: closing
        // it on its own would move someone's layout without them touching
        // it.
        if t.muerto() {
            // `soltar_terminal` and not `self.terminal = None`: it bumps the
            // epoch, and without that the tick that already rearmed above
            // would keep spinning at 30 Hz forever over a panel with no
            // shell — "spins at rest" again, this time triggered by the
            // shell leaving.
            self.soltar_terminal();
            return self.republicar_terminal();
        }
        if changed {
            return self.republicar_terminal();
        }
        Vec::new()
    }

    /// Releases the shell and stops its pump. Called by the slot's closing.
    pub(super) fn soltar_terminal(&mut self) {
        // `Shell`'s `Drop` kills the shell and waits for it.
        self.terminal = None;
        // And the epoch bumps: the tick in flight is left to die without
        // rearming.
        self.terminal_epoca += 1;
    }

    /// The keys when the terminal panel has focus: ALL to the shell, except
    /// the one that takes it out.
    ///
    /// `None` = this panel does not want them, and the key goes its normal
    /// way.
    ///
    /// It does not look like `tecla_en_preview` and it should not: that one
    /// resolves against a keymap, and here there is no keymap that counts —
    /// inside a shell, `tab`, the arrows and `ctrl+c` mean whatever the shell
    /// says. The only thing norte keeps is the lone chord that opened the
    /// panel; if the preset binds it to a sequence there is no door, and then
    /// the panel does NOT take the keys, which is better than a panel you
    /// cannot leave.
    pub(super) fn tecla_en_terminal(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let exit_chord = self.acorde_de_salida()?;
        let focus = self.roles.get(norte_frontend::layout::RoleId::Active)?;
        if super::kind_de(&self.arbol, focus).is_none_or(|k| k.as_str() != KIND) {
            return None;
        }
        let chord = k.to_chord().ok()?;
        if chord == exit_chord {
            // The SAME path that opened it: the key is one, so the way back
            // has to be the same code.
            return Some(self.abrir_panel_de_terminal(backend, mailbox));
        }
        let bytes = norte_frontend::subshell::chord_a_bytes(chord)?;
        self.terminal_escribir(&bytes);
        Some((self.aplicada(), Vec::new()))
    }

    /// The terminal slot's size in cells, without the frame.
    ///
    /// `None` if the slot is not placed — behind a tab, or does not fit: then
    /// the pty is not touched, because the last good size is better than a
    /// made-up one.
    fn tamano_del_terminal(&self) -> Option<(u16, u16)> {
        let id = self.hueco_de_kind(KIND)?;
        let (_, r) = self.reparto.placements.iter().find(|(s, _)| *s == id)?;
        Some((r.width.saturating_sub(2), r.height.saturating_sub(2)))
    }

    /// The LONE chord that runs `layout.terminal`, if the preset gives one.
    ///
    /// The rule that it has to be lone lives in `Effective::lone_chord` and is
    /// shared by the two places that hand the whole keyboard over to another
    /// program: the terminal's subshell and this panel. A two-key sequence
    /// would force stealing the shell's first key exactly where the reader is
    /// typing it.
    fn acorde_de_salida(&self) -> Option<norte_frontend::keymap::Chord> {
        self.efectivo.lone_chord(COMANDO)
    }

    /// Sends bytes to the shell, if there is one.
    pub(super) fn terminal_escribir(&mut self, bytes: &[u8]) {
        if let Some(t) = self.terminal.as_mut() {
            t.escribir(bytes);
        }
    }

    /// The whole snapshot, which is how any slot in this window republishes.
    fn republicar_terminal(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let snap = self.snapshot();
        vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// The panel's view for the snapshot, if the slot exists.
    pub(super) fn panel_de_terminal(&self, slot: u32) -> TerminalSlotView {
        build_view(
            slot,
            self.terminal.as_ref().map(norte_term::pty::Shell::pantalla),
        )
    }
}

/// Starts the shell with what norte decides: the program and the
/// environment.
///
/// These live here and not in `norte-term` because resolving the reader's
/// shell and the `NORTE_LEVEL` contract are norte's rules, not an emulator's.
fn arrancar(dir: &norte_proto::VPath) -> std::io::Result<norte_term::pty::Shell> {
    let native = norte_vfs::native::vpath_to_native(dir)
        .map_err(|_| std::io::Error::other("the directory is not a native path"))?;
    norte_term::pty::Shell::abrir(
        &norte_term::pty::Arranque {
            // `login_shell` refuses to return a relative `$SHELL` and falls
            // back to `/bin/sh` (#302): without that it would be looked up
            // via `cwd`, which here is the directory the reader is looking
            // at.
            programa: &norte_frontend::shell::login_shell(),
            dir: &native,
            // The real size is set by the renderer when it says which slot it
            // got; this is the startup one.
            tam: (80, 24),
            env: &[(
                norte_frontend::shell::LEVEL_VAR.into(),
                norte_frontend::shell::next_norte_level().into(),
            )],
        },
        norte_frontend::subshell::terminal_reply,
    )
}

/// A shell's grid, turned into the bridge's view.
///
/// Without a grid — there is no shell — the view SAYS so: a blank panel and a
/// panel with no shell look the same and are not the same thing.
#[must_use]
fn build_view(slot_id: u32, pantalla: Option<&Pantalla>) -> TerminalSlotView {
    let Some(p) = pantalla else {
        return TerminalSlotView {
            slot_id,
            rows: Vec::new(),
            cursor: None,
            no_shell: true,
        };
    };
    let (_, height) = p.tamano();
    TerminalSlotView {
        slot_id,
        rows: (0..height)
            .map(|f| {
                p.fila_tramos(f)
                    .into_iter()
                    .map(|(text, estilo)| span_view(text, estilo))
                    .collect()
            })
            .collect(),
        cursor: cursor_at(p),
        no_shell: false,
    }
}

/// Where the cursor goes, or `None` if the shell hid it.
///
/// Any full-screen program hides it while it paints, and then painting it
/// would be making up where it is.
fn cursor_at(p: &Pantalla) -> Option<(u16, u16)> {
    if !p.cursor_visible() {
        return None;
    }
    let (row, col) = p.cursor();
    let (width, height) = p.tamano();
    // The column can be worth as much as the width — the "pending wrap"
    // state — and there the cursor is painted in the last cell, which is
    // where a real terminal leaves it.
    (row < height).then(|| (row, col.min(width.saturating_sub(1))))
}

fn span_view(text: String, e: Estilo) -> TerminalSpanView {
    TerminalSpanView {
        text,
        fg: color_view(e.fg),
        bg: color_view(e.bg),
        bold: e.negrita,
        dim: e.tenue,
        italic: e.cursiva,
        underline: e.subrayado,
        reverse: e.inverso,
        strike: e.tachado,
    }
}

fn color_view(c: ColorTerm) -> Option<TerminalColorView> {
    match c {
        ColorTerm::PorDefecto => None,
        ColorTerm::Indexado(index) => Some(TerminalColorView::Indexed { index }),
        ColorTerm::Rgb(r, g, b) => Some(TerminalColorView::Rgb {
            hex: format!("#{r:02x}{g:02x}{b:02x}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An index crosses the bridge UNRESOLVED, and an RGB as hex.
    ///
    /// This is the module's decision, and it is pinned with a test because
    /// the day someone "improves" this by resolving the index against the
    /// theme, the panel will stop obeying the reader's theme and nothing else
    /// will turn red.
    #[test]
    fn an_index_stays_an_index_and_an_rgb_is_hex() {
        let mut p = Pantalla::nueva(12, 1);
        p.alimentar(b"\x1b[31ma\x1b[38;2;1;2;3mb");
        let v = build_view(7, Some(&p));
        let row = &v.rows[0];
        assert_eq!(
            row[0].fg,
            Some(TerminalColorView::Indexed { index: 1 }),
            "the shell's color 1 is not resolved here"
        );
        assert_eq!(
            row[1].fg,
            Some(TerminalColorView::Rgb {
                hex: "#010203".to_owned()
            })
        );
    }

    /// Without a shell, the view SAYS so instead of sending an empty grid.
    #[test]
    fn no_shell_says_so() {
        let v = build_view(3, None);
        assert!(v.no_shell);
        assert!(v.rows.is_empty());
        assert_eq!(v.cursor, None);
    }

    /// The cursor does not cross if the shell hid it.
    #[test]
    fn a_hidden_cursor_does_not_cross() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"hola");
        assert_eq!(build_view(1, Some(&p)).cursor, Some((0, 4)));
        p.alimentar(b"\x1b[?25l");
        assert_eq!(build_view(1, Some(&p)).cursor, None);
    }

    /// ALL the grid's rows go across, including the empty ones: a terminal
    /// does not scroll like a list, it repaints, and a renderer that only got
    /// the written ones would have to guess the height.
    #[test]
    fn all_rows_go_across() {
        let mut p = Pantalla::nueva(6, 4);
        p.alimentar(b"una");
        let v = build_view(1, Some(&p));
        assert_eq!(v.rows.len(), 4);
    }
}
