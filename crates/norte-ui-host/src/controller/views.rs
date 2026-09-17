//! Armar la foto y las vistas que la componen.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

/// Un fragmento del modelo compartido, en la forma del puente: el rol por su
/// nombre kebab (ya validado contra el tema), el color como `#rrggbb`, el
/// texto acotado. El enmascarado se hizo a la entrada
/// (`Viewer::with_plugin_preview_styled`), una sola vez.
pub(super) fn span_view(s: &norte_frontend::ansi::StyledSpan) -> crate::dto::SpanView {
    crate::dto::SpanView {
        text: clamp_display(s.text.clone()),
        role: s.role.map(|r| r.as_kebab().to_owned()),
        fg: s.fg.map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}")),
        bg: s.bg.map(|(r, g, b)| format!("#{r:02x}{g:02x}{b:02x}")),
    }
}

impl Estado {
    /// Proyecta un snapshot del daemon a lo que el renderer pinta.
    pub(super) fn vista_de(p: &norte_proto::TaskProgress) -> TaskView {
        // Por la regla COMPARTIDA, que cae a las entradas cuando no hay
        // bytes totales: esto solo miraba bytes, así que un borrado —que no
        // cuenta bytes— cruzaba el puente sin porcentaje de principio a fin.
        let porcentaje = norte_frontend::tasks::progress_pct(p);
        TaskView {
            task_id: p.task_id.get(),
            kind: clase_de_task(p.kind).to_owned(),
            state: match p.state {
                norte_proto::TaskState::Completed => TaskStateView::Done,
                norte_proto::TaskState::Cancelled => TaskStateView::Cancelled,
                norte_proto::TaskState::Failed { .. } => TaskStateView::Failed,
                norte_proto::TaskState::Running | norte_proto::TaskState::Paused => {
                    TaskStateView::Running
                }
                // Un estado que este host todavía no conoce se pinta como
                // encolado: es lo único que no miente sobre algo que sigue
                // vivo (`TaskState` es no exhaustivo por contrato del wire).
                _ => TaskStateView::Queued,
            },
            percent: porcentaje,
            // Vacíos aquí a propósito: el ritmo es de la TASK VIVA, que guarda
            // las fotos anteriores, y esta función solo ve una. Los rellena
            // `progreso`, que es quien tiene las dos.
            rate: String::new(),
            eta: String::new(),
            detail: p.current.as_ref().map(|path| {
                let (texto, _hostil) = norte_frontend::path_display(path);
                clamp_display(texto)
            }),
            detail_hostile: p
                .current
                .as_ref()
                .is_some_and(|path| norte_frontend::path_display(path).1),
            foreign: false,
        }
    }

    /// La pantalla entera: TODOS los huecos que el reparto pinta, cada uno
    /// proyectado según lo que es.
    ///
    /// Los ocultos no viajan. Un kind que este host todavía no proyecta sí
    /// viaja, en gris y con su nombre: preservar lo que no se entiende es la
    /// regla de la sesión (ADR 0059), y hacerlo desaparecer sería peor que
    /// enseñarlo apagado.
    pub(super) fn snapshot(&self) -> ViewSnapshot {
        let mut slots = Vec::new();
        for (slot, _) in &self.reparto.placements {
            let SlotId(id) = *slot;
            if let Some(hueco) = self.huecos.get(&id) {
                slots.push(SlotView::Browser(Box::new(self.browser(id, hueco))));
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
                Some("processes") => slots.push(SlotView::Processes {
                    slot_id: id,
                    // Índice sobre las filas PINTADAS, que es lo que el
                    // renderer resalta. Sobre el mapa entero, con el tablero
                    // recortado, señalaba a otra.
                    cursor: self.cursor_del_tablero(),
                }),
                // Un panel APORTADO por un plugin (fase 3), por PREFIJO: su
                // kind es `plugin:<id>:<kind>` y no se conoce al compilar, así
                // que no puede ser un brazo con su nombre como sus vecinos.
                // Y bien formado: `plugin:git` —con prefijo y sin la segunda
                // mitad— lo puede escribir una disposición a mano, y como
                // panel saldría sin título y sin líneas, o sea una caja muda.
                // Cayendo al brazo de abajo sale como lo que es: un kind que
                // este host no sabe pintar, con su nombre.
                Some(k) if k.starts_with("plugin:") && k.splitn(3, ':').count() == 3 => {
                    slots.push(SlotView::Panel(Box::new(self.vista_de_panel(id))));
                }
                _ => {
                    let nombre =
                        kind.map_or_else(|| "unknown".to_owned(), |k| k.as_str().to_owned());
                    // El kind sale de un fichero de disposición y `KindId` no
                    // valida nada: es texto que puede traer controles, y acaba en
                    // el DOM y en un `aria-label`.
                    let (pintable, hostil) = norte_frontend::display_name(nombre.as_bytes());
                    slots.push(SlotView::Unsupported {
                        slot_id: id,
                        kind_name: clamp_display(pintable),
                        kind_name_hostile: hostil,
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
            // Una foto REEMPLAZA lo que el renderer tenga, así que va
            // entera: un resync que se dejara fuera el diálogo abierto
            // dejaría al usuario mirando una pantalla sin la pregunta que
            // está esperando respuesta, con la operación destructiva todavía
            // viva. Lo mismo con el tablero.
            dialogs: self.vistas_de_dialogos(),
            tasks: self.vistas_de_tasks(),
            menu: self.vista_menu(),
            panel_bar: self.vista_barra_de_paneles(),
            key_bar: self.vista_barra_de_teclas(),
            profiles: self.vista_perfiles(),
            palette: self.vista_paleta(),
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
            locale: self.locale.clone(),
        }
    }

    /// Cuántas líneas se le mandan al visor y cuánto avanza una página.
    ///
    /// Lo dice el renderer (`SetViewerRows`); mientras no lo haya dicho, se
    /// estima con las celdas de la ventana menos el cromo. Es UN número para
    /// las dos cosas a propósito: cuando la estimación y lo que se pinta no
    /// coinciden, una página salta en silencio las líneas recortadas.
    pub(super) fn alto_del_visor(&self) -> usize {
        self.visor_filas
            .unwrap_or_else(|| usize::from(self.viewport.1.saturating_sub(2)))
            .max(1)
    }

    /// Las filas de la paleta: TODO lo que este host implementa.
    ///
    /// La descripción sale del catálogo Fluent compartido y el atajo del
    /// keymap efectivo, igual que en el TUI: una paleta construida de una
    /// lista a mano enseña atajos que el preset del usuario no tiene.
    pub(super) fn filas_de_paleta(&self) -> Vec<norte_frontend::palette::Row> {
        use norte_frontend::palette::first_chord;
        // Con los EFECTOS de esta ventana, no con todos: la paleta era la
        // única puerta que no pasaba por el keymap efectivo, así que una
        // ventana de solo lectura ofrecía copiar, mover y borrar. La guarda
        // de `aplicar_efecto` los rechazaba, pero ofrecer lo que se va a
        // rehusar es prometer algo que no se va a hacer.
        crate::commands::todos_con(self.efectos)
            .into_iter()
            .map(|cmd| norte_frontend::palette::Row {
                key: cmd.to_owned(),
                text: cmd.to_owned(),
                desc: norte_i18n::t_in(self.lang, &format!("help-cmd-{}", cmd.replace('.', "-"))),
                chord: first_chord(cmd, &self.efectivo)
                    .or_else(|| first_chord(cmd, self.resolver_visor_efectivo()))
                    .unwrap_or_else(|| "—".to_owned()),
                // Un comando propio es vocabulario de este proyecto.
                hostile: false,
            })
            .collect()
    }

    /// El efectivo del visor, para buscar el atajo de un comando suyo.
    pub(super) fn resolver_visor_efectivo(&self) -> &Effective {
        &self.efectivo_visor
    }

    /// Un click sobre una fila del selector de perfiles: la elige y la activa.
    ///
    /// La GENERACIÓN no es decorativa: la lista se llena desde una tarea de
    /// fondo, así que un índice de la pantalla anterior nombra otro perfil
    /// (ADR 0068). Una generación vieja se rechaza en vez de recortarse.
    pub(super) fn activar_perfil_de_fila(
        &mut self,
        row: u32,
        generation: u64,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if generation != self.gen_perfiles {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        }
        let Some(p) = self.selector_perfil.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let i = row as usize;
        let Some(fila) = p.rows().get(i) else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        if fila.problem.is_some() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-profile-broken".to_owned(),
                },
                Vec::new(),
            );
        }
        let nombre = fila.name.clone();
        let envios = self.elegir_perfil(&nombre, backend, buzon);
        (self.aplicada(), envios)
    }

    /// La proyección del selector de perfiles.
    pub(super) fn vista_perfiles(&self) -> Option<crate::dto::ProfilePickerView> {
        use norte_frontend::profile_picker::NameClash;
        let p = self.selector_perfil.as_ref()?;
        Some(crate::dto::ProfilePickerView {
            rows: p
                .rows()
                .iter()
                .map(|r| {
                    // El nombre son BYTES de un directorio: se enmascara, y se
                    // dice que se enmascaró (#266).
                    let (pintable, hostil) =
                        norte_frontend::display_os_name(std::path::Path::new(&r.name).as_os_str());
                    crate::dto::ProfileRowView {
                        name: clamp_display(pintable),
                        name_hostile: hostil,
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
                        // El diagnóstico sale de un fichero del usuario: va
                        // acotado y enmascarado como todo lo demás (#73).
                        problem: r.problem.clone().map(clamp_display).unwrap_or_default(),
                    }
                })
                .collect(),
            cursor: p.cursor() as u64,
            generation: self.gen_perfiles,
        })
    }

    /// La proyección de la barra de menús.
    ///
    /// Los títulos van SIEMPRE —la barra sigue ahí con el desplegable
    /// cerrado— y las entradas solo cuando hay uno abierto: un menú de doce
    /// entradas por cada uno de los siete, en cada parche, es media pantalla
    /// de JSON para pintar una fila de títulos.
    pub(super) fn vista_menu(&self) -> crate::dto::MenuView {
        use norte_frontend::menu::MENUS;
        let ejecutables = crate::commands::todos_con(self.efectos);
        let items = self.menu.as_ref().map_or_else(Vec::new, |m| {
            MENUS.get(m.menu()).map_or_else(Vec::new, |menu| {
                menu.items
                    .iter()
                    .map(|id| crate::dto::MenuItemView {
                        // La etiqueta CORTA y propia (`menu-item-*`), no la
                        // frase de `help-cmd-*`: esa es una descripción, y con
                        // ella el desplegable tapa los dos paneles. Mismo
                        // criterio que el TUI, que lo aprendió pintando.
                        label: clamp_display(norte_i18n::t_in(
                            self.lang,
                            &format!("menu-item-{}", id.replace('.', "-")),
                        )),
                        chord: clamp_display(
                            norte_frontend::palette::first_chord(id, &self.efectivo)
                                .or_else(|| {
                                    norte_frontend::palette::first_chord(id, &self.efectivo_visor)
                                })
                                .unwrap_or_else(|| "—".to_owned()),
                        ),
                        enabled: ejecutables.contains(id),
                    })
                    .collect()
            })
        });
        crate::dto::MenuView {
            // Por defecto ENCENDIDA, igual que el TUI: quien no ha dicho nada
            // no ha pedido esconderla.
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

    /// La barra de teclas (spec 2026-09-10): las diez celdas del keymap de
    /// la pantalla que tiene el teclado AHORA — el visor a pantalla completa
    /// si está, los listados si no. Con un diálogo delante la fila va EN
    /// BLANCO: ningún preset ata una tecla de función en `[dialog]`, y una
    /// celda que anunciara un verbo que el modal activo rehúsa sería la
    /// mentira que `hints` existe para no contar. El mismo orden que la TUI
    /// (`App::key_bar_cells`), y por eso las dos barras dicen lo mismo.
    pub(super) fn vista_barra_de_teclas(&self) -> crate::dto::KeyBarView {
        let bar = self.config.common.ui_chrome.key_bar();
        if !self.dialogos.is_empty() {
            return crate::dto::KeyBarView {
                bar,
                cells: Vec::new(),
            };
        }
        let eff = if self.visor.is_some() {
            &self.efectivo_visor
        } else {
            self.resolver.effective()
        };
        crate::dto::KeyBarView {
            bar,
            cells: norte_frontend::keybar::cells_in(eff, self.lang)
                .into_iter()
                .map(|c| crate::dto::KeyCellView {
                    key: u32::from(c.key),
                    label: clamp_display(c.label),
                    command: c.command.map(clamp_display),
                })
                .collect(),
        }
    }

    /// La proyección de la barra de paneles (#324).
    ///
    /// Lo que la TUI hace en `panel_buttons`, con lo que este host sabe: qué
    /// se COLOCÓ (del reparto, no del árbol — un hueco detrás de una pestaña
    /// o descartado por falta de sitio no está abierto, #329/#331), quién
    /// tiene el teclado, y qué tiene algo que contar sin estar a la vista.
    /// El QUÉ y el ORDEN son de `norte_frontend::panelbar`, compartidos.
    pub(super) fn vista_barra_de_paneles(&self) -> crate::dto::PanelBarView {
        let botones = self.botones_de_paneles();
        crate::dto::PanelBarView {
            // Por defecto ENCENDIDA, igual que la de menús y que la TUI.
            bar: self.config.common.ui_panel_bar.unwrap_or(true),
            names: self.config.common.ui_chrome.panel_bar_style()
                == norte_config::PanelBarStyle::Names,
            buttons: botones
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
                        attention: b.attention,
                    }
                })
                .collect(),
        }
    }

    /// Los botones de la barra, con su comando: lo que un click resuelve.
    pub(super) fn botones_de_paneles(&self) -> Vec<norte_frontend::panelbar::PanelButton> {
        // En ORDEN DE PANTALLA, que es el de los botones: de arriba abajo y,
        // a igual altura, de izquierda a derecha. El reparto los da en el
        // orden en que recorre el árbol, que casi siempre coincide y no lo
        // garantiza — y «casi siempre» no vale para una fila que se aprende
        // con el dedo.
        let mut placements: Vec<_> = self.reparto.placements.iter().collect();
        placements.sort_by_key(|(_, r)| (r.y, r.x));
        let colocados: Vec<String> = placements
            .iter()
            .filter_map(|(id, _)| kind_de(&self.arbol, *id))
            .map(|k| k.as_str().to_owned())
            .collect();
        let abiertos: Vec<&str> = colocados.iter().map(String::as_str).collect();
        // Un listado con el teclado no es «un panel enfocado»: la barra dice
        // a qué PANEL van las teclas, y a los listados van por defecto.
        let del_foco = kind_de(&self.arbol, SlotId(self.enfocado())).map(|k| k.as_str().to_owned());
        let focused = del_foco.as_deref().filter(|k| *k != "browser");
        // Novedad: el registro con avisos sin ver, y procesos con tareas en
        // el tablero. Con el panel A LA VISTA ya lo estás viendo: la marca
        // sobra. Mismo criterio que la TUI, y por eso se pregunta a los
        // colocados y no al árbol.
        let mut novedad: Vec<&str> = Vec::new();
        if !abiertos.contains(&"processes") && self.filas_de_tablero() > 0 {
            novedad.push("processes");
        }
        if !abiertos.contains(&super::logpanel::KIND)
            && self
                .log_ring
                .as_ref()
                .is_some_and(|r| r.has_at_or_above(norte_config::logline::LogLevel::Warn))
        {
            novedad.push(super::logpanel::KIND);
        }
        norte_frontend::panelbar::buttons_in(
            &self.kinds,
            norte_frontend::panelbar::PanelBarInput {
                open: &abiertos,
                focused,
                attention: &novedad,
            },
            self.lang,
        )
    }

    /// La proyección de la paleta.
    pub(super) fn vista_paleta(&self) -> Option<crate::dto::PaletteView> {
        let p = self.paleta.as_ref()?;
        let filas = p.rows();
        let visibles = p.visible();
        let sin_consulta = p.query_display().is_empty();
        Some(crate::dto::PaletteView {
            query: clamp_display(p.query_display()),
            rows: visibles
                .iter()
                // Un tope, como cualquier otra lista que cruza: con la
                // consulta vacía TODAS las filas son visibles, y las de
                // plugin las pone un tercero.
                .take(crate::bridge::MAX_ROWS_PER_BATCH)
                .filter_map(|i| filas.get(*i).map(|r| (*i, r)))
                .map(|(i, r)| crate::dto::PaletteRowView {
                    // Reciente solo mientras va arriba por serlo: con
                    // consulta el orden es el de lo que casa.
                    recent: sin_consulta && p.is_recent(i),
                    text: clamp_display(r.text.clone()),
                    desc: clamp_display(r.desc.clone()),
                    chord: clamp_display(r.chord.clone()),
                    // Lo que se pinta DIFIERE de lo que el manifiesto dice.
                    // Una fila de plugin es texto de tercero en la pantalla
                    // donde se elige qué código correr: sin esto se pintaba
                    // enmascarada y sin decirlo.
                    hostile: r.hostile,
                    // Los comandos propios los implementa este host —salen de
                    // su propia lista— y los de PLUGIN los resuelve el
                    // daemon, que exige aprobada + encendida por su cuenta.
                    enabled: true,
                })
                .collect(),
            cursor: (!visibles.is_empty()).then_some(p.cursor() as u64),
            total: filas.len() as u64,
        })
    }

    /// La proyección del panel de continuaciones.
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

    /// La proyección del visor, con la ventana de líneas que cabe.
    ///
    /// El alto sale del viewport en CELDAS —la misma rejilla que reparte la
    /// pantalla—, menos el cromo: el visor ocupa la ventana entera.
    pub(super) fn vista_visor(&self) -> Option<crate::dto::ViewerView> {
        let v = self.visor.as_ref()?;
        Some(self.vista_de_visor(v, self.alto_del_visor(), true))
    }

    /// La proyección de UN visor: el de pantalla completa o el de un hueco
    /// de preview (#291), que son el mismo modelo con otro vínculo.
    ///
    /// `con_imagen`: si una imagen aceptada se anuncia para que el renderer
    /// pida sus bytes. Solo el visor grande los sirve (`BytesDeImagen` es
    /// «la imagen del visor abierto»); en un hueco, una foto la pinta el
    /// previewer de imágenes con sus medios bloques, o se ve en crudo.
    pub(super) fn vista_de_visor(
        &self,
        v: &norte_frontend::viewer::Viewer,
        alto: usize,
        con_imagen: bool,
    ) -> crate::dto::ViewerView {
        let imagen = if con_imagen {
            Self::imagen_de(v)
        } else {
            Ok(None)
        };
        // El TUI pinta la ruta del visor con el encoding del panel ENFOCADO
        // (`ui::panels`), y por lo mismo: es el fichero que se abrió desde
        // ahí.
        let (path, hostil) =
            norte_frontend::path_display_with(&v.path, self.hueco().pane.name_encoding());
        crate::dto::ViewerView {
            path_display: clamp_display(path),
            path_hostile: hostil,
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
            lines: v.rows(alto).into_iter().map(clamp_display).collect(),
            // El nombre ya viene enmascarado del modelo compartido; se acota
            // aquí como todo lo que cruza.
            // Una MINIATURA de plugin (ADR 0107) manda sobre las dos cosas:
            // es la imagen que se anuncia, y el «via …» dice de quién es.
            // Solo en el visor grande (`con_imagen`), que es el único que
            // sirve bytes.
            preview_by: match (con_imagen, self.miniatura.as_ref()) {
                (true, Some((_, plugin))) => clamp_display(norte_i18n::ta_in(
                    self.lang,
                    "viewer-plugin-preview",
                    &[("plugin", plugin)],
                )),
                _ => v.preview_plugin().map_or_else(String::new, |n| {
                    // La MISMA clave que el TUI: el indicador «via …» no
                    // puede decirse de dos maneras según quién pinte. El
                    // nombre ya viene enmascarado del modelo compartido.
                    clamp_display(norte_i18n::ta_in(
                        self.lang,
                        "viewer-plugin-preview",
                        &[("plugin", n)],
                    ))
                }),
            },
            preview_lossy: v.preview_lossy(),
            image: match (con_imagen, self.miniatura.as_ref()) {
                (true, Some((vista, _))) => Some(vista.clone()),
                _ => imagen.clone().ok().flatten(),
            },
            image_refused: match &imagen {
                // Con miniatura, el motivo por el que el visor no pinta la
                // suya deja de importar: hay imagen.
                Err(clave) if !(con_imagen && self.miniatura.is_some()) => {
                    clamp_display(norte_i18n::t_in(self.lang, clave))
                }
                _ => String::new(),
            },
            // Los fragmentos con estilo de la MISMA ventana de filas que
            // `lines` (mismo `alto`, mismo `scroll`): una entrada por fila.
            // El texto ya llegó enmascarado del modelo compartido; se acota
            // aquí como todo lo que cruza.
            styled: v
                .plugin_styled_rows(alto)
                .map(|filas| {
                    filas
                        .iter()
                        .map(|linea| linea.iter().map(span_view).collect())
                        .collect()
                })
                .unwrap_or_default(),
        }
    }

    /// Si lo que hay en el visor es una imagen PINTABLE, y si no, por qué no.
    ///
    /// `Ok(None)` = no es una imagen. `Ok(Some(_))` = lo es y se acepta.
    /// `Err(clave)` = lo es y se RECHAZA, con la clave que lo explica.
    ///
    /// Los tres topes del ADR 0069, y los tres son negativas y no recortes:
    ///
    /// - El **formato** sale de los bytes mágicos, nunca de la extensión: una
    ///   extensión es una afirmación de quien nombró el fichero.
    /// - Las **dimensiones declaradas** se comparan con el presupuesto ANTES
    ///   de que nadie decodifique. Un PNG de 64 KB puede declarar 60000×60000
    ///   y costar gigabytes; leerle la cabecera es la única defensa barata.
    ///   Una cabecera que no se entiende también se rechaza: «no sé» tratado
    ///   como «adelante» es la puerta que esto existe para cerrar.
    /// - Los **bytes** los acota quien los sirve, y un fichero que no cabe no
    ///   se pinta A MEDIAS: media imagen decodificada es una imagen de otra
    ///   cosa.
    pub(super) fn imagen_de(
        v: &norte_frontend::viewer::Viewer,
    ) -> Result<Option<crate::dto::ImageView>, &'static str> {
        let Some(fmt) = v.image_kind() else {
            return Ok(None);
        };
        // La cabecera SIEMPRE cabe en lo que el visor ya leyó, así que
        // rechazar aquí no cuesta un viaje.
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
