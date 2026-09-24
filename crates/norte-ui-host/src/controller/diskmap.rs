//! The disk map, in the window (phase 4).
//!
//! The map's state is the SHARED one (`norte_frontend::diskmap`), the same
//! the terminal uses: which directory is described, what was measured, and
//! which child is chosen. And the layout into rectangles is shared too
//! (`norte_frontend::treemap::squarify`). What is here is the WIRING:
//! requesting the measurement, landing it, and resolving a click.
//!
//! The mold is the plugin panel's, on purpose: one state per slot, one live
//! request with its token, and a response that arrives with another token is
//! discarded. What changes is what is requested — a measurement, not a
//! frame — and that here what is kept between repaints is what was MEASURED,
//! which costs minutes.
//!
//! # Why the HOST lays out and not the renderer
//! A treemap computed twice is two different treemaps the moment someone
//! touches a rounding, and then the rectangle that is painted and the one
//! that resolves a click stop being the same one — i.e. you press one and the
//! one next to it opens. Same rule as the plugin panel (ADR 0077), and here
//! with more reason: what is on the other side of a click is a file.
//!
//! # The map does NOT follow the cursor
//! Its signature is the DIRECTORY, not the row. That is why it is in
//! `NO_SIGUEN`, and why moving the cursor does not re-measure: probing per
//! cursor would turn going down a `$HOME` into a storm of minutes-long
//! measurements.

use std::sync::Arc;

use norte_frontend::layout::SlotId;
use norte_proto::VPath;
use tokio::sync::mpsc;

use super::{Estado, Mensaje, RequestToken, kind_de};
use crate::backend::HostBackend;
use crate::bridge::{BridgeEnvelope, clamp_display};
use crate::dto::UiUpdate;

/// The kind that occupies a disk-map slot.
pub(super) const KIND: &str = "disk-map";

/// What a map slot has NOW and what it is requesting.
#[derive(Default)]
pub(super) struct EstadoMapa {
    /// What was measured, with its directory and its selection. The SHARED
    /// state.
    pub(super) mapa: norte_frontend::diskmap::DiskMap,
    /// The directory of the last measurement requested — or attempted and
    /// failed.
    ///
    /// Both things in one field because they answer the same question: does
    /// this need requesting? Without noting the failed attempt, a directory
    /// that cannot be measured would be retried after every actor message.
    pub(super) pedido: Option<VPath>,
    /// The request in flight, with its token.
    pub(super) en_vuelo: Option<(RequestToken, VPath)>,
}

impl Estado {
    /// Which directory the map in slot `slot` should be describing.
    ///
    /// The link is resolved with the shared engine, same as the preview, the
    /// viewer, and the plugin panel: a followed slot that dies degrades to
    /// the `active` role. It also returns the SLOT, because the click
    /// navigates THAT listing, not the map's.
    fn seguido_de_mapa(&self, slot: SlotId) -> Option<(u32, VPath)> {
        let mut diags = Vec::new();
        let followed =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active))
                .unwrap_or(SlotId(self.activo()));
        let SlotId(id) = followed;
        let slot_state = self.huecos.get(&id)?;
        Some((id, slot_state.pane.dir().clone()))
    }

    /// Requests the measurement for placed maps whose directory changed.
    ///
    /// It is called after EVERY actor message, like its neighbours, so the
    /// first thing is to bail out cheaply when there is nothing to do:
    /// walking the tree to discover there is no map at all is paid on every
    /// keystroke of every session that does not use it.
    pub(super) fn sondear_mapas(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let slots: Vec<SlotId> = self
            .reparto
            .placements
            .iter()
            .filter(|(slot, _)| kind_de(&self.arbol, *slot).is_some_and(|k| k.as_str() == KIND))
            .map(|(slot, _)| *slot)
            .collect();
        if slots.is_empty() && self.mapas.is_empty() {
            return Vec::new();
        }
        // A slot that no longer exists keeps nothing: a `SlotId` gets reused,
        // and without pruning, a new slot's map would inherit the previous
        // one's measurement — another directory's sizes, under this title.
        let alive: Vec<u32> = slots.iter().map(|SlotId(id)| *id).collect();
        self.mapas.retain(|id, _| alive.contains(id));

        for slot in slots {
            let SlotId(id) = slot;
            let Some((_, dir)) = self.seguido_de_mapa(slot) else {
                continue;
            };
            let state = self.mapas.entry(id).or_default();
            if state.pedido.as_ref() == Some(&dir) || state.en_vuelo.is_some() {
                continue;
            }
            // Pointing at it FORGETS what was measured: the previous
            // directory's map under the new one's title is the wrong answer
            // for exactly the while the measurement lasts, which is when
            // someone is looking at it.
            if state.mapa.dir() != Some(&dir) {
                state.mapa.apuntar(dir.clone());
            }
            self.token += 1;
            let token = RequestToken(self.token);
            self.mapas.entry(id).or_default().en_vuelo = Some((token, dir.clone()));

            let params = norte_proto::methods::FsDirUsageParams {
                path: dir.clone(),
                // One level: that is what a map paints, and it is the only
                // thing the server serves today. Asking for more is REJECTED
                // (ADR 0117).
                depth: 1,
            };
            let backend = Arc::clone(backend);
            let mailbox = mailbox.clone();
            tokio::spawn(async move {
                // The deadline governs the LAUNCH, not the measurement:
                // `fs.dir_usage` returns the Task as soon as it is queued,
                // and measuring a `$HOME` can take minutes. A deadline on the
                // measurement would kill it exactly on the trees for which it
                // exists.
                let launched =
                    match tokio::time::timeout(super::PLAZO_PLUGINS, backend.dir_usage(params))
                        .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(norte_proto::Error::ProviderUnavailable { retryable: true }),
                    };
                let task = match launched {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = mailbox
                            .send(Mensaje::MapaContenido(Box::new((id, token, Err(e)))))
                            .await;
                        return;
                    }
                };
                let task_id = task.id;
                let mut prog = task.progress;
                // The report is only DEFINITIVE once the Task is terminal.
                // Requesting it earlier would give half a map without saying
                // it is half, and half a map reads as a small directory.
                while !prog.borrow().state.is_terminal() {
                    if prog.changed().await.is_err() {
                        break;
                    }
                }
                let state_now = prog.borrow().state.clone();
                let res = if state_now.is_terminal() {
                    backend
                        .dir_usage_report(task_id)
                        .await
                        .map(|report| (state_now, report))
                } else {
                    // The channel died without reaching terminal: the daemon
                    // went down.
                    Err(norte_proto::Error::ProviderUnavailable { retryable: true })
                };
                let _ = mailbox
                    .send(Mensaje::MapaContenido(Box::new((id, token, res))))
                    .await;
            });
        }
        Vec::new()
    }

    /// Lands a measurement: it is shown if the token is that of THAT slot's
    /// last request, and discarded otherwise.
    ///
    /// **And the DIRECTORY is checked in addition to the token.** Measuring
    /// takes a while, and in that time the panel may be pointing elsewhere: a
    /// report landed without checking would paint one directory's sizes
    /// under another one's title.
    pub(super) fn aterrizar_mapa(
        &mut self,
        slot: u32,
        token: RequestToken,
        res: Result<
            (
                norte_proto::TaskState,
                norte_proto::methods::FsDirUsageReportResult,
            ),
            norte_proto::Error,
        >,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let state = self.mapas.get_mut(&slot)?;
        if state.en_vuelo.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        let (_, dir) = state.en_vuelo.take()?;
        // The attempt is recorded no matter what: without this, a directory
        // that cannot be measured would be retried after every actor
        // message.
        state.pedido = Some(dir.clone());
        if state.mapa.dir() != Some(&dir) {
            return None; // arrived late: the panel is already somewhere else
        }
        let (task_state, report) = match res {
            Ok(pair) => pair,
            Err(e) => {
                // The reason ends up in the panel's TITLE, so it goes
                // translated to the session's language and clamped, like
                // search's.
                state
                    .mapa
                    .fallo(clamp_display(norte_frontend::error::error_category_in(
                        self.lang, &e,
                    )));
                let snap = self.snapshot();
                return Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))));
            }
        };
        let complete = task_state == norte_proto::TaskState::Completed;
        state.mapa.aterrizar(report, complete);
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// A click on a rectangle: enters that child.
    ///
    /// It is resolved against the SAME layout that was painted —
    /// `vista_de_mapa` uses the size inside the border and so does this — so
    /// the rectangle that is seen and the one that answers are the same one
    /// by construction.
    ///
    /// **Without `zona_puede`**: that filter exists because in a plugin panel
    /// the label and the command are chosen by a third party and nothing
    /// binds them together. Here `squarify` sets them, so filtering them
    /// would be guarding against oneself.
    pub(super) fn clic_en_mapa(
        &mut self,
        slot: u32,
        row: u16,
        col: u16,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (crate::bridge::ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // A HIDDEN slot keeps its map, so its zones would keep resolving even
        // though nobody sees them. The renderer does not paint what is
        // hidden, so a click there does not come from a person.
        if self.oculto(slot) {
            return (Self::obsoleta(crate::StaleAction::Generation), Vec::new());
        }
        let Some((cols, rows)) = self
            .reparto
            .placements
            .iter()
            .find(|(SlotId(s), _)| *s == slot)
            .map(|(_, r)| (r.width.saturating_sub(2), r.height.saturating_sub(2)))
        else {
            return (self.aplicada(), Vec::new());
        };
        let chosen = self.mapas.get(&slot).and_then(|e| {
            let frame = norte_frontend::treemap::squarify(&e.mapa.informe().children, cols, rows);
            let arg = frame.hit_at(row, col)?.arg.clone()?;
            let seg = norte_proto::Segment::parse_wire(&arg).ok()?;
            // Only a DIRECTORY opens: the map shows both kinds, and
            // "entering" a file is not navigating.
            let child = e.mapa.informe().children.iter().find(|c| c.name == seg)?;
            (child.kind == norte_proto::EntryKind::Dir).then_some(seg)
        });
        let Some(seg) = chosen else {
            // A cell with no rectangle, or a file: nothing happens, and it is
            // not a reader error.
            return (self.aplicada(), Vec::new());
        };
        // Pointing ALSO selects: keyboard and mouse leave the map in the same
        // place, which is what makes clicking and then using the arrows
        // continue from where you were.
        if let Some(e) = self.mapas.get_mut(&slot) {
            e.mapa.elegir(&seg);
        }
        let Some((target_slot, dir)) = self.seguido_de_mapa(SlotId(slot)) else {
            return (self.aplicada(), Vec::new());
        };
        let target = dir.join(seg);
        // Navigate the FOLLOWED listing, not the map: the map points, and the
        // `cd` goes the same way as any other (ADR 0077). `Record` because
        // this is a move the reader asked for: it enters the trail and prunes
        // forward.
        let updates = self.navegar_hueco(
            target_slot,
            &target,
            norte_frontend::nav::Trail::Record,
            backend,
            mailbox,
        );
        (self.aplicada(), updates)
    }

    /// Projects a slot's disk map into what the renderer paints.
    ///
    /// The frame is laid out with the size INSIDE the border, same as a
    /// plugin panel's signature: whoever describes the content does not know
    /// where its slot landed, so the one who paints does the math — and here
    /// the host paints and resolves, so the two computations are the same
    /// one.
    ///
    /// Without a placed slot there is no size, and then there is no map: an
    /// empty one is sent with its title, like a panel whose first frame has
    /// not arrived yet.
    pub(super) fn vista_de_mapa(&self, id: u32) -> crate::dto::DiskMapSlotView {
        let state = self.mapas.get(&id);
        // The title is the NAME of the directory being described, not its
        // path: the slot is narrow and the whole path does not fit. It comes
        // from a file name, so it is masked like any other.
        let (title, title_hostile) = state.and_then(|e| e.mapa.dir()).map_or_else(
            || (String::new(), false),
            |d| {
                d.file_name().map_or_else(
                    // A provider's root has no base name: it is said with its
                    // scheme instead of leaving the title blank.
                    || (d.scheme().to_owned(), false),
                    |n| norte_frontend::display_name(n.as_bytes()),
                )
            },
        );

        let cells = self
            .reparto
            .placements
            .iter()
            .find(|(SlotId(s), _)| *s == id)
            .map(|(_, r)| (r.width.saturating_sub(2), r.height.saturating_sub(2)));

        let (lines, hits) = match (state, cells) {
            (Some(e), Some((cols, rows))) => {
                let frame =
                    norte_frontend::treemap::squarify(&e.mapa.informe().children, cols, rows);
                let lines = frame
                    .lines
                    .iter()
                    .map(|line| line.iter().map(super::views::span_view).collect())
                    .collect();
                let hits = frame
                    .hits
                    .iter()
                    .map(|h| crate::dto::HitView {
                        row: h.row,
                        col: h.col,
                        width: h.width,
                    })
                    .collect();
                (lines, hits)
            }
            _ => (Vec::new(), Vec::new()),
        };

        crate::dto::DiskMapSlotView {
            slot_id: id,
            title: clamp_display(title),
            title_hostile,
            lines,
            hits,
            measuring: state.is_some_and(|e| e.en_vuelo.is_some()),
        }
    }
}
