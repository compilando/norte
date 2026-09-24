//! Assembling the snapshot and the views that make it up.
//!
//! Part of `controller`: these are methods of `Estado`, moved here without
//! touching them (ADR 0086). The only writer is still the actor.

// These modules are the same `impl Estado` split into pieces, so they use
// the same imports as the parent. Listing them here would be a forty-line
// list per file, across 32 files, that goes out of sync the moment the
// parent imports something — `super::*` keeps it in sync on its own.
#[allow(clippy::wildcard_imports)]
use super::*;

/// A fragment of the shared model, in the bridge's shape: the role by its
/// kebab name (already validated against the theme), the color as
/// `#rrggbb`, the clamped text. The masking was done on the way in
/// (`Viewer::with_plugin_preview_styled`), once.
pub(super) fn span_view(s: &norte_frontend::ansi::StyledSpan) -> crate::dto::SpanView {
    crate::dto::SpanView {
        text: clamp_display(s.text.clone()),
        role: s.role.map(|r| r.as_kebab().to_owned()),
        fg: s.fg.map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}")),
        bg: s.bg.map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}")),
    }
}

impl Estado {
    /// Projects a daemon snapshot into what the renderer paints.
    pub(super) fn vista_de(p: &norte_proto::TaskProgress) -> TaskView {
        // By the SHARED rule, which falls back to entries when there are no
        // total bytes: this used to only look at bytes, so a delete — which
        // does not count bytes — crossed the bridge with no percentage from
        // start to finish.
        let percent = norte_frontend::tasks::progress_pct(p);
        TaskView {
            task_id: p.task_id.get(),
            kind: clase_de_task(p.kind).to_owned(),
            state: match p.state {
                norte_proto::TaskState::Completed => TaskStateView::Done,
                norte_proto::TaskState::Cancelled => TaskStateView::Cancelled,
                norte_proto::TaskState::Failed { .. } => TaskStateView::Failed,
                norte_proto::TaskState::Running => TaskStateView::Running,
                norte_proto::TaskState::Paused => TaskStateView::Paused,
                // A state this host does not know yet is painted as queued:
                // it is the only thing that does not lie about something
                // still alive (`TaskState` is non-exhaustive by the wire's
                // contract).
                _ => TaskStateView::Queued,
            },
            percent,
            // Empty here on purpose: the rate belongs to the LIVE task, which
            // keeps the previous snapshots, and this function only sees one.
            // `progreso` fills them in, since it has both.
            rate: String::new(),
            eta: String::new(),
            detail: p.current.as_ref().map(|path| {
                let (text, _hostile) = norte_frontend::path_display(path);
                clamp_display(text)
            }),
            detail_hostile: p
                .current
                .as_ref()
                .is_some_and(|path| norte_frontend::path_display(path).1),
            foreign: false,
        }
    }

    /// The whole screen: EVERY slot the layout paints, each projected
    /// according to what it is.
    ///
    /// Hidden ones do not travel. A kind this host does not yet project does
    /// travel, in gray and with its name: preserving what is not understood
    /// is the session's rule (ADR 0059), and making it disappear would be
    /// worse than showing it dimmed.
    pub(super) fn snapshot(&self) -> ViewSnapshot {
        let mut slots = Vec::new();
        for (slot, _) in &self.reparto.placements {
            let SlotId(id) = *slot;
            if let Some(slot_state) = self.huecos.get(&id) {
                slots.push(SlotView::Browser(Box::new(self.browser(id, slot_state))));
                continue;
            }
            let kind = kind_de(&self.arbol, *slot);
            match kind.as_ref().map(norte_frontend::layout::KindId::as_str) {
                Some("metadata") => {
                    slots.push(SlotView::Metadata(Box::new(self.hoja_de_atributos(*slot))));
                }
                Some("places") => slots.push(SlotView::Places(Box::new(self.barra_de_sitios(id)))),
                Some(super::preview::KIND) => {
                    slots.push(SlotView::Preview(Box::new(self.vista_de_preview(id))));
                }
                Some("tree") => slots.push(SlotView::Tree(Box::new(self.arbol_de_ramas(id)))),
                Some(super::logpanel::KIND) => {
                    slots.push(SlotView::Log(Box::new(self.panel_de_registro(id))));
                }
                Some(super::diskmap::KIND) => {
                    slots.push(SlotView::DiskMap(Box::new(self.vista_de_mapa(id))));
                }
                Some(super::timeline::KIND) => {
                    slots.push(SlotView::Timeline(Box::new(self.vista_de_linea(id))));
                }
                Some(super::termpanel::KIND) => {
                    slots.push(SlotView::Terminal(Box::new(self.panel_de_terminal(id))));
                }
                Some("processes") => slots.push(SlotView::Processes {
                    slot_id: id,
                    // Index over the PAINTED rows, which is what the renderer
                    // highlights. Over the whole map, with the board
                    // clamped, it pointed at a different one.
                    cursor: self.cursor_del_tablero(),
                }),
                // A panel CONTRIBUTED by a plugin (phase 3), by PREFIX: its
                // kind is `plugin:<id>:<kind>` and is not known at compile
                // time, so it cannot be an arm with its name like its
                // neighbors. And well-formed: `plugin:git` — with the prefix
                // and no second half — can be written by hand into a layout,
                // and as a panel it would come out with no title and no
                // lines, i.e. a mute box. Falling to the arm below, it comes
                // out as what it is: a kind this host does not know how to
                // paint, with its name.
                Some(k) if k.starts_with("plugin:") && k.splitn(3, ':').count() == 3 => {
                    slots.push(SlotView::Panel(Box::new(self.vista_de_panel(id))));
                }
                _ => {
                    let name = kind.map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
                    // The kind comes from a layout file and `KindId` validates
                    // nothing: it is text that can carry control characters,
                    // and ends up in the DOM and in an `aria-label`.
                    let (displayable, hostile) = norte_frontend::display_name(name.as_bytes());
                    slots.push(SlotView::Unsupported {
                        slot_id: id,
                        kind_name: clamp_display(displayable),
                        kind_name_hostile: hostile,
                    });
                }
            }
        }
        ViewSnapshot {
            compare: self.vista_comparacion(),
            sync: self.vista_sincronizacion(),
            connection: self.conexion.clone(),
            layout: self.disposicion(),
            slots,
            focus: Some(self.enfocado()),
            status: self.status.clone(),
            // A snapshot REPLACES whatever the renderer has, so it goes
            // whole: a resync that left out the open dialog would leave the
            // user looking at a screen with no question waiting for an
            // answer, with the destructive operation still alive. Same with
            // the board.
            dialogs: self.vistas_de_dialogos(),
            tasks: self.vistas_de_tasks(),
            menu: self.vista_menu(),
            panel_bar: self.vista_barra_de_paneles(),
            status_items: self.vista_elementos_de_estado(),
            layout_buttons: self.vista_botones_de_disposicion(),
            // The stripes (spec 2026-09-20). They go in the WHOLE view and
            // not in a patch: it is configuration, and a live reload rebuilds
            // the view.
            row_stripes: self.config.common.ui_chrome.row_stripes(),
            profiles: self.vista_perfiles(),
            palette: self.vista_paleta(),
            goto: self.vista_ir_a(),
            wizard: self.vista_asistente(),
            splash: self.vista_splash(),
            whichkey: self.vista_whichkey(),
            help: self.vista_ayuda(),
            settings: self.vista_ajustes(),
            extensions: self.vista_extensiones(),
            agents: self.vista_agentes(),
            plugin_output: self.escritorio.salida.clone(),
            program_output: self.escritorio.programa.clone(),
            theme: self.vista_tema(),
            search: self.vista_busqueda(),
            layouts: self.vista_disposiciones(),
            columns: self.vista_columnas(),
            picker: self.vista_selector(),
            viewer: self.vista_visor(),
            ai_rename: self.vista_ia(),
            organize: self.vista_organizar(),
            locale: self.locale.clone(),
        }
    }

    /// How many lines are sent to the viewer and how far a page advances.
    ///
    /// The renderer says so (`SetViewerRows`); until it has, it is estimated
    /// from the window's cells minus the chrome. It is ONE number for both
    /// things on purpose: when the estimate and what is painted disagree, a
    /// page silently skips the clamped lines.
    pub(super) fn alto_del_visor(&self) -> usize {
        self.visor_filas
            .unwrap_or_else(|| usize::from(self.viewport.1.saturating_sub(2)))
            .max(1)
    }

    /// The palette's rows: EVERYTHING this host implements.
    ///
    /// The description comes from the shared Fluent catalogue and the
    /// shortcut from the effective keymap, same as in the TUI: a palette
    /// built from a hand-written list shows shortcuts the user's preset does
    /// not have.
    pub(super) fn filas_de_paleta(&self) -> Vec<norte_frontend::palette::Row> {
        use norte_frontend::palette::first_chord;
        // With THIS window's effects, not with all of them: the palette used
        // to be the only door that did not go through the effective keymap,
        // so a read-only window offered copy, move and delete. `aplicar_
        // efecto`'s guard rejected them, but offering what is going to be
        // refused is promising something that is not going to happen.
        crate::commands::todos_con(self.efectos)
            .into_iter()
            .map(|cmd| norte_frontend::palette::Row {
                key: cmd.to_owned(),
                text: cmd.to_owned(),
                desc: norte_i18n::t_in(self.lang, &format!("help-cmd-{}", cmd.replace('.', "-"))),
                chord: first_chord(cmd, &self.efectivo)
                    .or_else(|| first_chord(cmd, self.resolver_visor_efectivo()))
                    .unwrap_or_else(|| "—".to_owned()),
                // A command of our own is this project's vocabulary.
                hostile: false,
            })
            .collect()
    }

    /// The viewer's effective keymap, to look up one of its commands' key.
    pub(super) fn resolver_visor_efectivo(&self) -> &Effective {
        &self.efectivo_visor
    }

    /// A click on a row of the profile selector: selects it and activates it.
    ///
    /// The GENERATION is not decorative: the list fills in from a background
    /// task, so an index from the previous screen names a different profile
    /// (ADR 0068). An old generation is rejected instead of clamped.
    pub(super) fn activar_perfil_de_fila(
        &mut self,
        row: u32,
        generation: u64,
        backend: &Arc<dyn HostBackend>,
        mailbox: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_perfiles {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(p) = self.selector_perfil.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let i = row as usize;
        let Some(row) = p.rows().get(i) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        if row.problem.is_some() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-profile-broken".to_owned(),
                },
                Vec::new(),
            );
        }
        let name = row.name.clone();
        let outgoing = self.elegir_perfil(&name, backend, mailbox);
        (self.aplicada(), outgoing)
    }

    /// The profile selector's projection.
    pub(super) fn vista_perfiles(&self) -> Option<crate::dto::ProfilePickerView> {
        use norte_frontend::profile_picker::NameClash;
        let p = self.selector_perfil.as_ref()?;
        Some(crate::dto::ProfilePickerView {
            rows: p
                .rows()
                .iter()
                .map(|r| {
                    // The name is a directory's BYTES: it is masked, and it
                    // is said that it was masked (#266).
                    let (displayable, hostile) =
                        norte_frontend::display_os_name(std::path::Path::new(&r.name).as_os_str());
                    crate::dto::ProfileRowView {
                        name: clamp_display(displayable),
                        name_hostile: hostile,
                        title: r.title.clone().map(clamp_display),
                        active: r.active,
                        clash: match r.clash {
                            NameClash::None => String::new(),
                            NameClash::Layout => {
                                norte_i18n::t_in(self.lang, "profile-picker-clash-layout")
                            }
                            NameClash::Keymap => {
                                norte_i18n::t_in(self.lang, "profile-picker-clash-keymap")
                            }
                            NameClash::Both => {
                                norte_i18n::t_in(self.lang, "profile-picker-clash-both")
                            }
                        },
                        no_state: !r.carries_state,
                        // The diagnostic comes from a user file: it is
                        // clamped and masked like everything else (#73).
                        problem: r.problem.clone().map(clamp_display).unwrap_or_default(),
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            generation: self.gen_perfiles,
        })
    }

    /// The menu bar's projection.
    ///
    /// Titles ALWAYS go — the bar is still there with the dropdown closed —
    /// and the entries only when one is open: a menu of twelve entries for
    /// each of the seven, on every patch, is half a screen of JSON to paint
    /// one row of titles.
    pub(super) fn vista_menu(&self) -> crate::dto::MenuView {
        use norte_frontend::menu::MENUS;
        let runnable = crate::commands::todos_con(self.efectos);
        let items = self.menu.as_ref().map_or_else(Vec::new, |m| {
            MENUS.get(m.menu()).map_or_else(Vec::new, |menu| {
                menu.items()
                    .enumerate()
                    .map(|(i, id)| crate::dto::MenuItemView {
                        // The SHORT, own label (`menu-item-*`), not the
                        // `help-cmd-*` phrase: that one is a description, and
                        // with it the dropdown covers both panels. Same
                        // criterion as the TUI, which learned it by painting.
                        label: clamp_display(norte_i18n::t_in(
                            self.lang,
                            &format!("menu-item-{}", id.replace('.', "-")),
                        )),
                        chord: clamp_display(
                            norte_frontend::palette::first_chord(id, &self.efectivo)
                                .or_else(|| {
                                    norte_frontend::palette::first_chord(id, &self.efectivo_visor)
                                })
                                .unwrap_or_default(),
                        ),
                        enabled: runnable.contains(&id),
                        section: menu.section_at(i).map(|title| {
                            title.map_or_else(String::new, |k| {
                                clamp_display(norte_i18n::t_in(self.lang, k))
                            })
                        }),
                        role: norte_frontend::menu::role(id).as_str().to_owned(),
                    })
                    .collect()
            })
        });
        crate::dto::MenuView {
            // ON by default, same as the TUI: whoever has said nothing has
            // not asked to hide it.
            bar: self.config.common.ui_menu_bar.unwrap_or(true),
            titles: MENUS
                .iter()
                .map(|m| clamp_display(norte_i18n::t_in(self.lang, m.title)))
                .collect(),
            open: self.menu.as_ref().map(|m| m.menu() as u64),
            cursor: self.menu.as_ref().map_or(0, |m| m.item() as u64),
            items,
        }
    }

    /// The panel bar's projection (#324).
    ///
    /// What the TUI does in `panel_buttons`, with what this host knows: what
    /// got PLACED (from the layout, not the tree — a slot behind a tab or
    /// dropped for lack of room is not open, #329/#331), who has the
    /// keyboard, and what has something to report without being in view. The
    /// WHAT and the ORDER belong to `norte_frontend::panelbar`, shared.
    pub(super) fn vista_barra_de_paneles(&self) -> crate::dto::PanelBarView {
        let buttons = self.botones_de_paneles();
        crate::dto::PanelBarView {
            // ON by default, same as the menu bar's and the TUI's.
            bar: self.config.common.ui_panel_bar.unwrap_or(true),
            names: self.config.common.ui_chrome.panel_bar_style().shows_names(),
            // `auto` = column: the window is short on height, not width.
            vertical: self
                .config
                .common
                .ui_chrome
                .panel_bar_position()
                .vertical(true),
            buttons: buttons
                .iter()
                .map(|b| {
                    let (kind, _) = norte_frontend::display_name(b.kind.as_bytes());
                    crate::dto::PanelButtonView {
                        label: clamp_display(norte_frontend::panelbar::label_in(
                            self.lang, &b.kind, &b.command,
                        )),
                        kind,
                        letter: b.letter.to_string(),
                        chord: clamp_display(
                            norte_frontend::palette::first_chord(&b.command, &self.efectivo)
                                .unwrap_or_else(|| "—".to_owned()),
                        ),
                        state: match b.state {
                            norte_frontend::panelbar::PanelState::Closed => {
                                crate::dto::PanelButtonState::Closed
                            }
                            norte_frontend::panelbar::PanelState::Open => {
                                crate::dto::PanelButtonState::Open
                            }
                            norte_frontend::panelbar::PanelState::Focused => {
                                crate::dto::PanelButtonState::Focused
                            }
                        },
                        attention: b.attention > 0,
                        count: b.attention,
                    }
                })
                .collect(),
        }
    }

    /// The status bar's items, UNCLAMPED by width: what a click resolves.
    /// From the same code as the TUI (ADR 0132).
    pub(super) fn elementos_de_estado(&self) -> Vec<norte_frontend::statusbar::StatusItemView> {
        let input = norte_frontend::statusbar::StatusInput::from_pane(
            &self.hueco().pane,
            self.tira.view(self.reloj_tira()),
            self.status.notices_unread,
        );
        // The plugins' first (ADR 0137), like in the TUI: to the left of the
        // right half, and the first to give way.
        let mut list = norte_frontend::statusbar::plugin_items(
            &self.hueco().pane,
            &self.config.common.ui_status_plugins,
            self.lang,
        );
        list.extend(norte_frontend::statusbar::items(
            &input,
            self.config.common.ui_chrome.status_items(),
            self.lang,
        ));
        list
    }

    /// The status bar's right half's projection (ADR 0132): what fits in half
    /// the declared width, dropped by priority with the same `fit` as the
    /// TUI's.
    pub(super) fn vista_elementos_de_estado(&self) -> Vec<crate::dto::StatusItemView> {
        let list = self.elementos_de_estado();
        let width = usize::from(self.viewport.0);
        norte_frontend::statusbar::fit(&list, width / 2, 2)
            .into_iter()
            .map(|v| {
                use norte_frontend::task_strip::StripPhase;
                crate::dto::StatusItemView {
                    id: clamp_display(v.id.clone()),
                    text: clamp_display(v.text.clone()),
                    tooltip: clamp_display(v.tooltip.clone()),
                    clickable: v.command.is_some(),
                    progress: v.progress.filter(|_| v.bar).map(|p| {
                        crate::dto::StatusProgressView {
                            percent: p.percent,
                            phase: match p.phase {
                                StripPhase::Running => "running",
                                StripPhase::Paused => "paused",
                                StripPhase::Done => "done",
                                StripPhase::Failed => "failed",
                            }
                            .to_owned(),
                        }
                    }),
                }
            })
            .collect()
    }

    /// The layout buttons (ADR 0133), with their menu entry's name and the
    /// LIVE keymap's shortcut. They go in the snapshot: the keymap changes
    /// with a profile or a reload, and both send a snapshot.
    pub(super) fn vista_botones_de_disposicion(&self) -> Vec<crate::dto::ChromeButtonView> {
        norte_frontend::layoutbar::BUTTONS
            .iter()
            .map(|b| crate::dto::ChromeButtonView {
                id: b.id.to_owned(),
                label: clamp_display(norte_frontend::layoutbar::label(b, self.lang)),
                chord: clamp_display(
                    norte_frontend::palette::first_chord(b.command, &self.efectivo)
                        .unwrap_or_else(|| "—".to_owned()),
                ),
            })
            .collect()
    }

    /// The bar's buttons, with their command: what a click resolves.
    pub(super) fn botones_de_paneles(&self) -> Vec<norte_frontend::panelbar::PanelButton> {
        // In SCREEN ORDER, which is the buttons' order: top to bottom and, at
        // the same height, left to right. The layout gives them in the order
        // it walks the tree, which almost always matches and does not
        // guarantee it — and "almost always" is no good for a row learned by
        // finger memory.
        let mut placements: Vec<_> = self.reparto.placements.iter().collect();
        placements.sort_by_key(|(_, r)| (r.y, r.x));
        let placed: Vec<String> = placements
            .iter()
            .filter_map(|(id, _)| kind_de(&self.arbol, *id))
            .map(|k| k.as_str().to_owned())
            .collect();
        let open_kinds: Vec<&str> = placed.iter().map(String::as_str).collect();
        // A listing with the keyboard is not "a focused panel": the bar says
        // which PANEL the keys go to, and listings get them by default.
        let focused_kind =
            kind_de(&self.arbol, SlotId(self.enfocado())).map(|k| k.as_str().to_owned());
        let focused = focused_kind.as_deref().filter(|k| *k != "browser");
        // News: the log with unseen warnings, and processes with tasks on the
        // board. With the panel IN VIEW you are already seeing it: the mark
        // is redundant. Same criterion as the TUI's, and that is why it asks
        // the placed ones and not the tree.
        let mut attention_list: Vec<(&str, u32)> = Vec::new();
        if !open_kinds.contains(&"processes") {
            attention_list.push((
                "processes",
                norte_frontend::panelbar::cifra(self.filas_de_tablero()),
            ));
        }
        if !open_kinds.contains(&super::logpanel::KIND)
            && let Some(ring) = self.log_ring.as_ref()
        {
            attention_list.push((
                super::logpanel::KIND,
                norte_frontend::panelbar::cifra(
                    ring.count_at_or_above(norte_config::logline::LogLevel::Warn),
                ),
            ));
        }
        norte_frontend::panelbar::buttons_in(
            &self.kinds,
            norte_frontend::panelbar::PanelBarInput {
                open: &open_kinds,
                focused,
                attention: &attention_list,
            },
            self.lang,
        )
    }

    /// The palette's projection.
    pub(super) fn vista_paleta(&self) -> Option<crate::dto::PaletteView> {
        let p = self.paleta.as_ref()?;
        let rows = p.rows();
        let visible = p.visible();
        let no_query = p.query_display().is_empty();
        Some(crate::dto::PaletteView {
            query: clamp_display(p.query_display()),
            rows: visible
                .iter()
                // A cap, like any other list that crosses: with an empty
                // query EVERY row is visible, and the plugin ones are put
                // there by a third party.
                .take(crate::bridge::MAX_ROWS_PER_BATCH)
                .filter_map(|i| rows.get(*i).map(|r| (*i, r)))
                .map(|(i, r)| crate::dto::PaletteRowView {
                    // Recent only while it is up top for being one: with a
                    // query the order is by what matches.
                    recent: no_query && p.is_recent(i),
                    text: clamp_display(r.text.clone()),
                    desc: clamp_display(r.desc.clone()),
                    chord: clamp_display(r.chord.clone()),
                    // What is painted DIFFERS from what the manifest says. A
                    // plugin row is a third party's text on the screen where
                    // it is chosen what code to run: without this it painted
                    // masked and without saying so.
                    hostile: r.hostile,
                    // This host implements its OWN commands — they come from
                    // its own list — and the PLUGIN ones are resolved by the
                    // daemon, which requires approved + enabled on its own.
                    enabled: true,
                })
                .collect(),
            cursor: (!visible.is_empty()).then_some(p.cursor() as u64),
            total: rows.len() as u64,
        })
    }

    /// The continuations panel's projection.
    pub(super) fn vista_whichkey(&self) -> Option<crate::dto::WhichKeyView> {
        let panel = self.whichkey.as_ref()?;
        Some(crate::dto::WhichKeyView {
            title: clamp_display(panel.title.clone()),
            rows: panel
                .rows
                .iter()
                .map(|r| crate::dto::WhichKeyRowView {
                    chord: clamp_display(r.chord.clone()),
                    label: clamp_display(r.label.clone()),
                    enabled: r.avail == Availability::Here,
                    opens_sequence: r.opens_sequence,
                    reason: clamp_display(r.reason.clone()),
                })
                .collect(),
        })
    }

    /// The viewer's projection, with the window of lines that fits.
    ///
    /// The height comes from the viewport in CELLS — the same grid that
    /// splits the screen — minus the chrome: the viewer occupies the whole
    /// window.
    pub(super) fn vista_visor(&self) -> Option<crate::dto::ViewerView> {
        let v = self.visor.as_ref()?;
        Some(self.vista_de_visor(v, self.alto_del_visor(), true))
    }

    /// A SINGLE viewer's projection: the full-screen one or a preview slot's
    /// (#291), which are the same model with a different link.
    ///
    /// `con_imagen`: whether an accepted image is announced so the renderer
    /// requests its bytes. Only the big viewer serves them
    /// (`BytesDeImagen` is "the open viewer's image"); in a slot, a photo is
    /// painted by the image previewer with its half-blocks, or seen raw.
    pub(super) fn vista_de_visor(
        &self,
        v: &norte_frontend::viewer::Viewer,
        height: usize,
        con_imagen: bool,
    ) -> crate::dto::ViewerView {
        let image = if con_imagen {
            Self::imagen_de(v)
        } else {
            Ok(None)
        };
        // The TUI paints the viewer's path with the FOCUSED panel's encoding
        // (`ui::panels`), and for the same reason: it is the file that was
        // opened from there.
        let (path, hostile) =
            norte_frontend::path_display_with(&v.path, self.hueco().pane.name_encoding());
        crate::dto::ViewerView {
            path_display: clamp_display(path),
            path_hostile: hostile,
            encoding: v.encoding_name().to_owned(),
            eol: match v.eol() {
                norte_encoding::Eol::Lf => "lf",
                norte_encoding::Eol::CrLf => "crlf",
                norte_encoding::Eol::Cr => "cr",
                norte_encoding::Eol::Mixed => "mixed",
                norte_encoding::Eol::None => "none",
            }
            .to_owned(),
            hex: v.hex,
            forced: v.is_forced(),
            had_errors: v.had_errors(),
            truncated: v.truncated,
            total_rows: v.total_rows() as u64,
            first_line: v.scroll as u64,
            total_cols: v.max_cols() as u64,
            first_col: v.hscroll() as u64,
            lines: v.rows(height).into_iter().map(clamp_display).collect(),
            // The name already arrives masked from the shared model; it is
            // clamped here like everything that crosses.
            // A plugin THUMBNAIL (ADR 0107) rules over both things: it is the
            // image that is announced, and the "via …" says whose it is.
            // Only in the big viewer (`con_imagen`), which is the only one
            // that serves bytes.
            preview_by: match (con_imagen, self.miniatura.as_ref()) {
                (true, Some((_, plugin))) => clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "viewer-plugin-preview",
                    &[("plugin", plugin)],
                )),
                _ => v.preview_plugin().map_or_else(String::new, |n| {
                    // The SAME key as the TUI's: the "via …" indicator cannot
                    // be said two different ways depending on who paints it.
                    // The name already arrives masked from the shared model.
                    clamp_display(norte_i18n::ta_in(
                        self.lang,
                        "viewer-plugin-preview",
                        &[("plugin", n)],
                    ))
                }),
            },
            preview_lossy: v.preview_lossy(),
            image: match (con_imagen, self.miniatura.as_ref()) {
                (true, Some((view, _))) => Some(view.clone()),
                _ => image.clone().ok().flatten(),
            },
            image_refused: match &image {
                // With a thumbnail, the reason the viewer does not paint its
                // own stops mattering: there is an image.
                Err(key) if !(con_imagen && self.miniatura.is_some()) => {
                    clamp_display(norte_i18n::t_in(self.lang, key))
                }
                _ => String::new(),
            },
            // The zoom (bridge 80). It is the viewer's own state, same as
            // hex or forced encoding, so it comes from it.
            image_zoom: v.zoom_pct(),
            // The styled fragments from the SAME row window as `lines` (same
            // `height`, same `scroll`): one entry per row. The text already
            // arrived masked from the shared model; it is clamped here like
            // everything that crosses.
            styled: v
                .plugin_styled_rows(height)
                .map(|rows| {
                    rows.iter()
                        .map(|line| line.iter().map(span_view).collect())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Whether what is in the viewer is a PAINTABLE image, and if not, why
    /// not.
    ///
    /// `Ok(None)` = it is not an image. `Ok(Some(_))` = it is one and it is
    /// accepted. `Err(key)` = it is one and it is REJECTED, with the key that
    /// explains it.
    ///
    /// ADR 0069's three caps, and all three are refusals and not truncations:
    ///
    /// - The **format** comes from the magic bytes, never the extension: an
    ///   extension is a claim made by whoever named the file.
    /// - The **declared dimensions** are compared against the budget BEFORE
    ///   anyone decodes. A 64 KB PNG can declare 60000×60000 and cost
    ///   gigabytes; reading its header is the only cheap defense. A header
    ///   that is not understood is also rejected: "I don't know" treated as
    ///   "go ahead" is the door this exists to close.
    /// - The **bytes** are capped by whoever serves them, and a file that
    ///   does not fit is not painted HALFWAY: half a decoded image is an
    ///   image of something else.
    pub(super) fn imagen_de(
        v: &norte_frontend::viewer::Viewer,
    ) -> Result<Option<crate::dto::ImageView>, &'static str> {
        let Some(fmt) = v.image_kind() else {
            return Ok(None);
        };
        // The header ALWAYS fits in what the viewer already read, so
        // rejecting here costs no trip.
        let bytes = v.image_bytes().unwrap_or_default();
        let Some((w, h)) = norte_frontend::viewer::image_dimensions(bytes) else {
            return Err("viewer-image-unreadable");
        };
        if u64::from(w) * u64::from(h) > norte_frontend::viewer::PIXEL_BUDGET {
            return Err("viewer-image-too-large");
        }
        Ok(Some(crate::dto::ImageView {
            format: fmt.label().to_owned(),
            width: w,
            height: h,
        }))
    }
}
