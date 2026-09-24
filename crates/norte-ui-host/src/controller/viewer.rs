//! The file and image viewer.
//!
//! Part of `controller`: these are methods of `State`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl State` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

impl State {
    /// The keys while the viewer is open.
    ///
    /// They resolve with the `viewer` screen's map, and whatever is not bound
    /// there does NOT fall through to the listing: an open viewer that let
    /// `F8` through would be a delete with the screen covered.
    pub(super) fn key_in_viewer(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Ok(chord) = k.to_chord() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        };
        let Resolution::Run { command, count } = self.resolver_visor.push(chord) else {
            // Half-finished prefix, a count, or nothing: the viewer has no
            // status bar of its own yet, so there is nothing to paint.
            return (self.applied(), Vec::new());
        };
        if command == "app.help" {
            // Help belongs to the APPLICATION and not to the viewer, so it is
            // not in its command list — and it cannot be: the two lists are
            // disjoint on purpose. It is handled here so that `F1` with the
            // viewer open opens the viewer's help page instead of answering
            // "not here".
            return self.open_help(backend, mailbox);
        }
        let Some(effect) = crate::commands::viewer_effect_of(&command, count.times()) else {
            // In the catalogue and bound to this screen, but this host does
            // not do it: it is said, with the same phrase as the TUI.
            let phrase = norte_frontend::keymap::unavailable_message_in(
                &command,
                Availability::NotHere,
                self.lang,
            );
            self.status.message = Some(clamp_display(phrase));
            let change = ViewChange::Status(self.status.clone());
            return (
                ActionAck::Unavailable {
                    reason_key: "cmd-not-here".to_owned(),
                },
                vec![self.parche(vec![change])],
            );
        };
        // Siblings are handled BEFORE borrowing the viewer: they do not move
        // it, they open ANOTHER file, so they need the whole state.
        if let crate::commands::EffectVisor::Sibling { forward } = effect {
            return self.viewer_sibling(forward, backend, mailbox);
        }
        let height = self.alto_del_visor();
        let Some(v) = self.visor.as_mut() else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        // `unsigned_abs`, not `abs`: the wheel's delta arrives RAW from the
        // renderer, and `i64::MIN.abs()` overflows — panics in debug, wraps
        // in release — meaning a malformed message would bring the host
        // down.
        let steps = |n: i64| usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
        match effect {
            crate::commands::EffectVisor::Close => {
                self.visor = None;
                self.viewer_in_flight = None;
                self.visor_token = None;
                // The image is RELEASED on closing: it is megabytes, and a
                // closed viewer has nothing to show.
                self.imagen = None;
                self.thumbnail = None;
            }
            crate::commands::EffectVisor::Line(n) if n < 0 => v.scroll_up(steps(n)),
            crate::commands::EffectVisor::Line(n) => v.scroll_down(steps(n)),
            crate::commands::EffectVisor::Page(n) if n < 0 => {
                v.scroll_up(steps(n).saturating_mul(height));
            }
            crate::commands::EffectVisor::Page(n) => {
                v.scroll_down(steps(n).saturating_mul(height));
            }
            crate::commands::EffectVisor::Column(n) if n < 0 => v.scroll_left(steps(n)),
            crate::commands::EffectVisor::Column(n) => v.scroll_right(steps(n)),
            crate::commands::EffectVisor::Extremo { al_final: false } => v.scroll_top(),
            crate::commands::EffectVisor::Extremo { al_final: true } => v.scroll_bottom(),
            crate::commands::EffectVisor::Hex => v.toggle_hex(),
            crate::commands::EffectVisor::Encoding => v.cycle_encoding(),
            crate::commands::EffectVisor::EncodingAuto => v.reset_encoding(),
            crate::commands::EffectVisor::Zoom { zoom_in: true } => v.zoom_in(),
            crate::commands::EffectVisor::Zoom { zoom_in: false } => v.zoom_out(),
            crate::commands::EffectVisor::ZoomFit => v.zoom_fit(),
            // Handled above, BEFORE borrowing the viewer: it opens another
            // file instead of moving this one, so it never reaches here. The
            // arm exists because the compiler requires covering the variant,
            // and does nothing because there is nothing to move.
            crate::commands::EffectVisor::Sibling { .. } => {}
        }
        // A viewer PATCH. The whole snapshot used to send, for every scroll
        // line, the visible rows of every listing underneath.
        let change = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// The WHEEL over the full-screen viewer (bridge 59).
    ///
    /// Both axes, because a single gesture produces them: the plain wheel
    /// goes down, with `shift` it goes sideways. A viewer patch and not a
    /// snapshot, for the same reason as the keys: the whole snapshot would
    /// send, on every turn, the visible rows of every listing underneath that
    /// nobody sees.
    pub(super) fn scroll_visor(
        &mut self,
        lines: i64,
        columns: i64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(v) = self.visor.as_mut() else {
            return (Self::stale(StaleAction::Modal), Vec::new());
        };
        // `unsigned_abs`, not `abs`: the wheel's delta arrives RAW from the
        // renderer, and `i64::MIN.abs()` overflows — panics in debug, wraps
        // in release — meaning a malformed message would bring the host
        // down.
        let steps = |n: i64| usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
        if lines < 0 {
            v.scroll_up(steps(lines));
        } else {
            v.scroll_down(steps(lines));
        }
        if columns < 0 {
            v.scroll_left(steps(columns));
        } else {
            v.scroll_right(steps(columns));
        }
        let change = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.applied(), vec![self.parche(vec![change])])
    }

    /// Stores a scheme's attribute catalogue and repaints.
    ///
    /// Sends a SNAPSHOT and not a patch: the catalogue changes how cells that
    /// already travelled are read — a mode that arrived as a number and is
    /// now `rwx` — and that is not a row change, it is a different reading of
    /// everything there is.
    pub(super) fn apply_catalog(
        &mut self,
        scheme: String,
        catalog: norte_proto::AttrCatalog,
    ) -> BridgeEnvelope<UiUpdate> {
        self.catalogos.insert(scheme, catalog);
        let snap = self.snapshot();
        self.over(UiUpdate::Snapshot(Box::new(snap)))
    }

    /// Requests the content of the entry under the cursor to open the viewer.
    pub(super) fn request_visor(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entry) = self.slot().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-to-view".to_owned(),
                },
                Vec::new(),
            );
        };
        if entry.kind == EntryKind::Dir {
            // Viewing a directory is entering it, and that already has its
            // own key.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-cannot-view-dir".to_owned(),
                },
                Vec::new(),
            );
        }
        self.request_viewer_of(entry.path, backend, mailbox)
    }

    /// The same, but for an EXPLICIT path instead of the cursor's row.
    ///
    /// It exists for the viewer's siblings: `viewer.next` opens a row that is
    /// NOT the selected one — under a quick-search filter, "the selected one"
    /// is not even the cursor's row — so the path is brought by whoever chose
    /// it, and it is not resolved again here.
    pub(super) fn request_viewer_of(
        &mut self,
        path: VPath,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.token += 1;
        let token = RequestToken(self.token);
        self.viewer_in_flight = Some(token);
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        tokio::spawn(async move {
            // One byte more than the budget: that is what gives away that the
            // file kept going. The rest is NOT read.
            let reading = backend.read(
                path.clone(),
                Some(norte_proto::ByteRange {
                    offset: 0,
                    len: Some(VISOR_CAP + 1),
                }),
            );
            // With a deadline: a hung mount cannot leave the F3 key with no
            // outcome forever.
            let read_bytes = match tokio::time::timeout(DEADLINE_VISOR, reading).await {
                Ok(r) => r,
                // The wire has no "timed out"; what happened is a read that
                // did not arrive, and to the user that is the same as a
                // provider that does not answer.
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            // It opens RIGHT AWAY with what was read. Plugins are asked
            // afterwards (`request_style`, ADR 0141): the viewer used to wait
            // for the previewer to open, and a previewer takes however long
            // it takes — with compilation in the mix, seconds per F3.
            let _ = mailbox
                .send(Message::Content(Box::new((token, path, read_bytes, None))))
                .await;
        });
        (self.applied(), Vec::new())
    }

    /// Opens the next (or previous) sibling of the same class, without
    /// leaving.
    ///
    /// Three decisions worth noting:
    ///
    /// - **The starting row is looked up by PATH, not by the cursor.** Under a
    ///   quick-search filter "the selected one" is not the cursor's row, and
    ///   the viewer may have opened from exactly there; asking the cursor
    ///   would give another row's sibling.
    /// - **The VIEWER decides the class**, which it knows from the bytes it
    ///   already read ([`norte_frontend::viewer::Viewer::is_image`]): a photo
    ///   saved as `.dat` still leads to the next photo.
    /// - **The cursor moves to the sibling**, and that is why on closing the
    ///   viewer the listing is where the reader was looking, not where they
    ///   entered.
    fn viewer_sibling(
        &mut self,
        forward: bool,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(v) = self.visor.as_ref() else {
            return (Self::stale(StaleAction::Generation), Vec::new());
        };
        let wanted = if v.is_image_by_bytes() {
            norte_frontend::viewer::Class::Imagen
        } else {
            norte_frontend::viewer::Class::Other
        };
        let open_path = v.path.clone();
        let pane = &self.slot().pane;
        let entries = pane.entries();
        // Only by what the reader SEES: with a live filter, the ladder is the
        // filter's and not the whole listing's.
        let visible = pane.quick_visible();
        let target = entries
            .iter()
            .position(|e| e.path == open_path)
            .and_then(|from| {
                norte_frontend::viewer::sibling(entries, visible, from, forward, wanted)
            })
            .and_then(|i| entries.get(i).map(|e| (i, e.path.clone())));
        let Some((row, path)) = target else {
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "host-no-sibling",
            )));
            let change = ViewChange::Status(self.status.clone());
            return (
                ActionAck::Unavailable {
                    reason_key: "host-no-sibling".to_owned(),
                },
                vec![self.parche(vec![change])],
            );
        };
        // An earlier "no more" cannot survive a jump that DID happen.
        self.status.message = None;
        self.slot_mut().pane.point_at(row);
        self.request_viewer_of(path, backend, mailbox)
    }

    /// Requests the STYLED view of the viewer's file from plugins, without
    /// making anyone wait (ADR 0141): the viewer is already open with the raw
    /// one, and this replaces it if it arrives in time and the viewer is
    /// still the same one. A previewer that fails, that takes too long, or
    /// that does not apply is NOT an error: the raw one stays, which is what
    /// the TUI already does.
    fn request_style(
        &self,
        path: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let path = path.clone();
        // The viewer's width, in cells, for the previewer (proto 0.66.0): the
        // one the renderer measured, or the viewport if it has not painted it
        // yet.
        let columns = Some(self.visor_columns.unwrap_or(u32::from(self.viewport.0)));
        tokio::spawn(async move {
            let preview = match tokio::time::timeout(
                DEADLINE_PLUGINS,
                backend.plugin_preview_styled(path, columns),
            )
            .await
            {
                Ok(Ok(p)) => p,
                _ => None,
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::Style(
                    token, preview,
                ))))
                .await;
        });
    }

    /// The styled view, arrived: it replaces the raw one if the viewer is
    /// still the one that requested it, at the same line it was at.
    pub(super) fn apply_style(
        &mut self,
        token: RequestToken,
        preview: Option<norte_proto::methods::PluginPreviewStyled>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_token != Some(token) {
            return None;
        }
        let p = preview?;
        let current = self.visor.as_ref()?;
        // If the reader already CHOSE how to view it — hex, another encoding
        // — while the style was arriving, it is respected: replacing the
        // viewer would undo it without saying anything.
        if current.hex || current.is_forced() {
            return None;
        }
        let path = current.path.clone();
        let scroll_at = current.scroll;
        // The by-bytes verdict travels from the old viewer to the new one:
        // the style replaces what is PAINTED, not what the file IS.
        let by_bytes = current.is_image_by_bytes();
        let mut new_viewer = norte_frontend::viewer::Viewer::with_plugin_preview_styled(
            path,
            p.plugin_name,
            &p.lines,
            p.lossy,
        );
        new_viewer.set_image_by_bytes(by_bytes);
        // Where the reader already was: they may have scrolled down while the
        // style was arriving. It is the SAME ROW, not always the same line of
        // the file: a previewer that splits a long line in two shifts what is
        // below.
        new_viewer.scroll = scroll_at.min(new_viewer.total_rows().saturating_sub(1));
        self.visor = Some(new_viewer);
        let change = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        Some(self.parche(vec![change]))
    }

    /// Opens the viewer with what was read.
    pub(super) fn open_visor(
        &mut self,
        token: RequestToken,
        path: VPath,
        read_bytes: Result<Vec<u8>, Error>,
        preview: Option<norte_proto::methods::PluginPreviewStyled>,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.viewer_in_flight != Some(token) {
            // The user closed the viewer, requested another file, or moved
            // elsewhere while this was in flight. Opening it now would be
            // opening a window nobody asked for — and switching their
            // keyboard's map.
            return None;
        }
        self.viewer_in_flight = None;
        self.visor_token = Some(token);
        // A new viewer: the previous one's image is no longer needed. And it
        // has to be released, not just stop being painted: it is megabytes.
        self.imagen = None;
        self.thumbnail = None;
        match read_bytes {
            Ok(mut bytes) => {
                let cap = usize::try_from(VISOR_CAP).unwrap_or(usize::MAX);
                let truncated = bytes.len() > cap;
                if truncated {
                    bytes.truncate(cap);
                }
                let target_path = path.clone();
                // By BYTES, before the previewer: see `set_image_by_bytes`.
                let by_bytes = norte_frontend::viewer::image_format(&bytes).is_some();
                let mut opened = match preview {
                    // A previewer applied: ITS OWN is shown. The bytes
                    // already read are not thrown away — they were needed to
                    // know the file can be read — but they are not painted:
                    // painting both would be showing the same file twice.
                    Some(p) => norte_frontend::viewer::Viewer::with_plugin_preview_styled(
                        path,
                        p.plugin_name,
                        &p.lines,
                        p.lossy,
                    ),
                    None => norte_frontend::viewer::Viewer::new(path, bytes, truncated),
                };
                opened.set_image_by_bytes(by_bytes);
                self.visor = Some(opened);
                // An image the window paints ON ITS OWN goes through no
                // plugin (ADR 0141): neither the styled view — which turned
                // it into ANSI art and left it with no image of its own — nor
                // the thumbnail. That was two plugins compiled just to open a
                // photo.
                let own_image = self
                    .visor
                    .as_ref()
                    .is_some_and(|v| matches!(Self::imagen_de(v), Ok(Some(_))));
                self.request_imagen(&target_path, token, backend, mailbox);
                if !own_image {
                    self.request_thumbnail(&target_path, token, backend, mailbox);
                    self.request_style(&target_path, token, backend, mailbox);
                }
            }
            Err(e) => {
                // Could not be read: it is SAID, instead of opening an empty
                // viewer that looks like a zero-byte file.
                // An error's text can come from a newer peer (`LimitExceeded`
                // with an unknown token, a host fingerprint) and ends up in
                // the DOM: it is masked like any other foreign text.
                let (displayable, _) = norte_frontend::display_name(format!("{e}").as_bytes());
                self.status.message = Some(clamp_display(displayable));
                let change = ViewChange::Status(self.status.clone());
                return Some(self.parche(vec![change]));
            }
        }
        let snap = self.snapshot();
        Some(self.over(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Brings the image's WHOLE bytes, if the viewer has an accepted one.
    ///
    /// The header was already read with the viewer and already said yes;
    /// this brings the rest. If the file fit in what was already read there
    /// is no second trip: the bytes are already there.
    ///
    /// The cap is a REFUSAL, not a truncation. Half a decoded image is an
    /// image of something else, so a file above [`IMAGEN_CAP`] is not painted
    /// and it is said.
    pub(super) fn request_imagen(
        &mut self,
        path: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        let Some(v) = self.visor.as_ref() else {
            return;
        };
        if !matches!(Self::imagen_de(v), Ok(Some(_))) {
            return;
        }
        if !v.truncated {
            // It fit whole in the viewer's read: there is nothing to
            // request.
            self.imagen = v.image_bytes().map(|b| std::sync::Arc::new(b.to_vec()));
            return;
        }
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let path = path.clone();
        tokio::spawn(async move {
            // One byte more than the cap: that is what gives away it does not
            // fit.
            let reading = backend.read(
                path,
                Some(norte_proto::ByteRange {
                    offset: 0,
                    len: Some(IMAGEN_CAP + 1),
                }),
            );
            let read_bytes = match tokio::time::timeout(DEADLINE_VISOR, reading).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::Imagen(
                    token, read_bytes,
                ))))
                .await;
        });
    }

    /// The image's bytes, arrived.
    ///
    /// Discarded if the viewer is already a different one: painting the
    /// previous picture over the current file is the same class of error as
    /// opening a viewer nobody asked for.
    // TODO(translation): review — this paragraph documents `apply_imagen`
    /// below, but the item right after it is `request_thumbnail`'s doc, about
    /// requesting a plugin thumbnail; it looks like a stale fragment left by
    /// an earlier edit.
    /// Asks a plugin for the viewer's file's THUMBNAIL (ADR 0107), and only
    /// when the viewer has no image of its own to paint: a format the webview
    /// does not decode, or an image that does not fit its caps. With its own
    /// image, nobody is bothered. The requested side is the viewer's height
    /// in estimated pixels (`height` rows × 22), bounded.
    pub(super) fn request_thumbnail(
        &mut self,
        path: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Message>,
    ) {
        // The ceiling is the plugin-host's (`THUMB_MAX_EDGE`), repeated here
        // because this crate does not know it: it bounds the same on both
        // sides.
        const THUMBNAIL_MAX_EDGE: u32 = 2048;
        let Some(v) = self.visor.as_ref() else {
            return;
        };
        if matches!(Self::imagen_de(v), Ok(Some(_))) {
            return;
        }
        let rows = u32::from(self.viewport.1).max(10);
        let max_edge = (rows * 22).clamp(128, THUMBNAIL_MAX_EDGE);
        let backend = Arc::clone(backend);
        let mailbox = mailbox.clone();
        let path = path.clone();
        tokio::spawn(async move {
            let thumb = match tokio::time::timeout(
                DEADLINE_PLUGINS,
                backend.plugin_thumbnail(path, max_edge),
            )
            .await
            {
                Ok(Ok(t)) => t,
                _ => None,
            };
            let _ = mailbox
                .send(Message::Background(Box::new(Background::Thumbnail(
                    token, thumb,
                ))))
                .await;
        });
    }

    /// The thumbnail arrived (or did not): with it, the viewer announces it
    /// as an image and says whose it is; without it, nothing changes. The
    /// bytes already come verified by the plugin-host (ADR 0107 decision 3);
    /// here only the plugin's name is clamped, which is its own text.
    pub(super) fn apply_thumbnail(
        &mut self,
        token: RequestToken,
        thumb: Option<norte_proto::methods::PluginThumbnail>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_token != Some(token) {
            return None;
        }
        let t = thumb?;
        if t.bytes.is_empty() || t.width == 0 || t.height == 0 {
            return None;
        }
        let (name, _) = norte_frontend::display_name(t.plugin_name.as_bytes());
        self.imagen = Some(std::sync::Arc::new(t.bytes));
        self.thumbnail = Some((
            crate::dto::ImageView {
                // The label the viewer paints for its own image is the
                // format in uppercase (`PNG`); the same shape here.
                format: t
                    .mimetype
                    .rsplit('/')
                    .next()
                    .unwrap_or("image")
                    .to_ascii_uppercase(),
                width: t.width,
                height: t.height,
            },
            clamp_display(name),
        ));
        let change = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        Some(self.parche(vec![change]))
    }

    pub(super) fn apply_imagen(
        &mut self,
        token: RequestToken,
        read_bytes: Result<Vec<u8>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_token != Some(token) {
            return None;
        }
        let Ok(bytes) = read_bytes else {
            return None;
        };
        if bytes.len() as u64 > IMAGEN_CAP {
            // Does not fit. It is said and the raw view is shown: showing it
            // halfway would be showing a different image.
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "viewer-image-too-large",
            )));
            let change = ViewChange::Status(self.status.clone());
            return Some(self.parche(vec![change]));
        }
        self.imagen = Some(std::sync::Arc::new(bytes));
        let change = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        Some(self.parche(vec![change]))
    }
}
