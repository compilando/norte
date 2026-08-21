//! La ayuda (F1) vista desde el host: qué página está abierta y cómo se
//! proyecta para quien la pinta.
//!
//! Nada de esto es nuevo. El corpus y su modelo de bloques son de
//! `norte-help`, el overlay —lateral, historial, filtro, qué es ejecutable—
//! es `norte_frontend::help::HelpState`, la resolución de las marcas vivas es
//! `norte_frontend::help_chords::Chords` y la hoja de teclado es
//! `norte_frontend::keysheet`. Lo que este módulo aporta es la PROYECCIÓN:
//! convertir todo eso en el vocabulario cerrado que cruza el bridge, para que
//! el renderer construya nodos del DOM uno a uno y no interprete marcado.
//!
//! Dos cosas se congelan al ABRIR y no se recalculan al pintar, por el mismo
//! motivo que el panel de continuaciones se construye en la transición: los
//! hechos con los que se atenúa una fila (`Facts`) y la hoja de teclado, que
//! son varias cadenas y un formato Fluent por fila. Congelar los hechos
//! además evita que una página se contradiga a mitad de lectura —una fila
//! atenuada arriba y viva abajo porque el cursor se movió por debajo—, que es
//! la misma decisión que tomó el TUI.

use std::collections::{HashMap, HashSet};

use norte_frontend::help::{Action, Focus, HelpState, KEYS_ID, PluginNode, SidebarRow};
use norte_frontend::help_badge::plugin_label;
use norte_frontend::help_chords::Chords;
use norte_frontend::keymap::{Availability, Effective, Screen, short_unavailable_message};
use norte_frontend::keysheet::sheet;
use norte_help::{Block, Span, TopicId, rows_of};
use norte_i18n::Lang;

use crate::bridge::clamp_display;
use crate::dto::{
    HelpActionView, HelpBlockView, HelpFocusView, HelpKeyRowView, HelpSidebarRowView, HelpSpanView,
    HelpView,
};

/// La ayuda abierta.
pub(crate) struct Ayuda {
    /// El modelo compartido: qué página, qué cursor, qué filtro.
    pub(crate) estado: HelpState,
    /// El resolver con el que se pinta ESTA apertura, con sus hechos ya
    /// congelados.
    chords: Chords,
    /// La hoja de teclado, generada una vez con el mapa efectivo del lector.
    teclas: Vec<HelpBlockView>,
    /// A quién atribuir la página de cada plugin, ya enmascarado y acotado.
    ///
    /// Sale de la FOTO del catálogo y jamás de la página: un plugin no decide
    /// quién lo publica. Un publicador en blanco no entra —la insignia
    /// pintaría «publicado por » sin nada detrás, que se lee como un fallo de
    /// pintado y no como una ausencia.
    publicadores: HashMap<String, String>,
    /// Las páginas de plugin que ya se han PEDIDO en esta apertura.
    ///
    /// «Se pide una vez» es de aquí y no del modelo: `plugin_needs_fetch` es
    /// una pregunta de SONDEO, y sin esta memoria un daemon muerto se
    /// re-preguntaría en cada proyección.
    pedidas: HashSet<String>,
}

impl Ayuda {
    /// Abre la ayuda sobre la página del CONTEXTO donde está el lector, o
    /// sobre el índice si ese contexto no tiene página.
    ///
    /// El contexto es una palabra del vocabulario cerrado del corpus y
    /// siempre hay una: quién la reclama lo dice cada página en su portada,
    /// así que una página nueva para un diálogo nuevo no toca este código.
    ///
    /// La página del contexto se abre como RAÍZ (`open_as_root`): el lector
    /// no navegó hasta ella, la ayuda lo puso ahí, así que «volver» no puede
    /// llevarle a un índice donde nunca estuvo — y una sola pulsación de
    /// `Esc` tiene que dejar una página que nadie pidió.
    pub(crate) fn abrir(
        lang: Lang,
        contexto: &str,
        listado: &Effective,
        visor: &Effective,
        facts: norte_frontend::availability::Facts,
    ) -> Self {
        let mut estado = HelpState::new(lang, norte_i18n::t_in(lang, "help-topic-keys"));
        if let Some(t) = norte_help::topic_for_context(lang, contexto) {
            estado.open_as_root(&t.id);
        }
        Self {
            estado,
            publicadores: HashMap::new(),
            pedidas: HashSet::new(),
            // DOS pantallas y no tres: los diálogos de esta ventana los
            // contesta el renderer con sus propios botones, así que no hay
            // mapa `dialog` en el que resolver un verbo suyo y ponerle una
            // tecla sería ponerla en una página que nadie va a pulsar.
            chords: Chords::over(&[listado, visor], lang).with_facts(facts),
            teclas: hoja_de_teclado(listado, visor, lang),
        }
    }

    /// La proyección entera.
    ///
    /// `efectos` y `visor_abierto` son las dos mitades de la pregunta «¿puede
    /// ESTA ventana correr esta fila?», que no es la misma que «¿se puede
    /// ahora?» — ver [`veredicto_del_host`].
    pub(crate) fn vista(
        &self,
        lang: Lang,
        efectos: crate::commands::Efectos,
        visor_abierto: bool,
    ) -> HelpView {
        let topic = self.estado.current_topic();
        let en_teclas = self.estado.current().as_str() == KEYS_ID;
        // Una página de extensión que todavía no ha llegado no tiene `Topic`,
        // y ahí es donde estaba el agujero: sin título y SIN LÍNEA DE
        // PROCEDENCIA, o sea con la forma exacta de una página del binario.
        // El nodo de la lateral sí sabe cómo se llama, y que es de un
        // tercero, así que la página en vuelo se pinta con las dos cosas.
        let en_vuelo = topic
            .is_none()
            .then(|| self.estado.plugin_needs_fetch())
            .flatten();
        let titulo = if en_teclas {
            norte_i18n::t_in(lang, "help-topic-keys")
        } else if let Some(id) = en_vuelo {
            self.titulo_de_nodo(id)
        } else {
            topic.map_or_else(String::new, |t| t.title.clone())
        };
        let filas = self.filas(lang, efectos, visor_abierto);
        HelpView {
            title: clamp_display(titulo),
            // NO pasa por `clamp_display`: es una CLAVE, y recortar no es
            // inyectivo. Entera o vacía.
            topic_id: identidad(self.estado.current().as_str()),
            badge: self.insignia(lang, topic, en_vuelo),
            sidebar: self.lateral(lang),
            cursor: self.estado.cursor() as u64,
            focus: match self.estado.focus() {
                Focus::Topics => HelpFocusView::Topics,
                Focus::Body => HelpFocusView::Body,
            },
            blocks: if en_teclas {
                self.teclas.clone()
            } else {
                topic.map_or_else(Vec::new, |t| {
                    t.blocks.iter().map(|b| bloque(b, &self.chords)).collect()
                })
            },
            actions: filas.into_iter().map(|(_, v, _)| v).collect(),
            action_cursor: (!self.estado.actions().is_empty())
                .then_some(self.estado.action_cursor() as u64),
            filter: clamp_display(self.estado.filter_display()),
            filtering: self.estado.filtering(),
            can_back: self.estado.can_back(),
        }
    }

    /// El título con el que la lateral nombra a `id`.
    ///
    /// Sale del NODO y no de la página, porque la página es justo lo que no
    /// ha llegado. Ya viene enmascarado y acotado de la entrada.
    fn titulo_de_nodo(&self, id: &str) -> String {
        self.estado
            .rows()
            .iter()
            .find_map(|r| match r {
                SidebarRow::Topic { id: this, title } if this.as_str() == id => Some(title.clone()),
                _ => None,
            })
            .unwrap_or_else(|| id.to_owned())
    }

    /// La línea de procedencia, si la página es de un tercero.
    ///
    /// Una página de extensión SIEMPRE la lleva, también mientras se está
    /// pidiendo: una línea que aparece a veces enseña lo contrario de la
    /// verdad cuando falta, y quien la lee está decidiendo si aprueba la
    /// extensión. Con la página en vuelo no se sabe todavía si se recortó ni
    /// si hubo bytes que no decodificaron, así que se dicen las dos que sí se
    /// saben: que es de una extensión y quién la publica.
    fn insignia(
        &self,
        lang: Lang,
        topic: Option<&norte_help::Topic>,
        en_vuelo: Option<&str>,
    ) -> Option<String> {
        if let Some(id) = en_vuelo {
            return norte_frontend::help_badge::plugin_badge(
                self.publicadores.get(id).map(String::as_str),
                false,
                false,
                lang,
            )
            .map(clamp_display);
        }
        match &topic?.origin {
            norte_help::Origin::BuiltIn => None,
            norte_help::Origin::Plugin {
                publisher,
                truncated,
                lossy,
                ..
            } => norte_frontend::help_badge::plugin_badge(
                publisher.as_deref(),
                *truncated,
                *lossy,
                lang,
            )
            .map(clamp_display),
        }
    }

    /// La lateral, con las cabeceras de grupo ya traducidas.
    fn lateral(&self, lang: Lang) -> Vec<HelpSidebarRowView> {
        let actual = self.estado.current();
        self.estado
            .rows()
            .iter()
            .map(|r| match r {
                SidebarRow::Group { tag } => HelpSidebarRowView::Group {
                    label: clamp_display(etiqueta_de_grupo(tag, lang)),
                },
                SidebarRow::Topic { id, title } => HelpSidebarRowView::Topic {
                    title: clamp_display(title.clone()),
                    current: id == actual,
                },
            })
            .collect()
    }

    /// Las filas ejecutables: la acción del MODELO y su proyección, juntas.
    ///
    /// Una sola pasada produce las dos a propósito. `HelpActivate{index}`
    /// indexa el modelo con un índice que salió de la proyección, así que dos
    /// recorridos separados eran dos listas que podían dejar de coincidir sin
    /// que nada lo dijera — y entonces un click en «copiar» corre otra cosa.
    /// El orden es el de [`HelpState::actions`]: primero los comandos que la
    /// página documenta, luego sus enlaces de «ver también».
    fn filas(
        &self,
        lang: Lang,
        efectos: crate::commands::Efectos,
        visor_abierto: bool,
    ) -> Vec<(Action, HelpActionView, Option<&'static str>)> {
        let Some(topic) = self.estado.current_topic() else {
            return Vec::new();
        };
        let mut out: Vec<(Action, HelpActionView, Option<&'static str>)> =
            rows_of(topic, &self.chords)
                .into_iter()
                .map(|r| {
                    let motivo = motivo_de(&r.row, efectos, visor_abierto);
                    let vista = HelpActionView {
                        // El nombre de un comando puede venir de una capa de
                        // keymap del usuario o del proyecto, que no lleva
                        // confianza, y `label_or_id` cae al id crudo cuando el
                        // catálogo no lo nombra.
                        label: clamp_display(norte_frontend::display_name(r.label.as_bytes()).0),
                        chord: clamp_display(r.chord.unwrap_or_default()),
                        enabled: motivo.is_none(),
                        reason: clamp_display(
                            motivo.map_or_else(String::new, |k| norte_i18n::t_in(lang, k)),
                        ),
                        opens_topic: false,
                    };
                    (Action::Run(r.row.command), vista, motivo)
                })
                .collect();
        out.extend(topic.see_also.iter().map(|id| {
            let vista = HelpActionView {
                label: clamp_display(titulo_de(id, self.estado.lang())),
                chord: String::new(),
                // Un enlace siempre se puede seguir: lo único que hace es
                // cambiar de página, y si el corpus de este idioma no la
                // tiene, el propio modelo lo ignora sin panicar.
                enabled: true,
                reason: String::new(),
                opens_topic: true,
            };
            (Action::Open(id.clone()), vista, None)
        }));
        out
    }

    /// Qué hace la acción `i`, si esta ventana puede hacerla.
    ///
    /// `Err` es la clave Fluent del motivo. La comprueba el HOST y no el
    /// renderer: si la comprobación viviera solo en quien pinta, el camino
    /// del teclado —que no pasa por ahí— correría una fila atenuada.
    pub(crate) fn accion_ejecutable(
        &self,
        i: usize,
        lang: Lang,
        efectos: crate::commands::Efectos,
        visor_abierto: bool,
    ) -> Result<Action, String> {
        let filas = self.filas(lang, efectos, visor_abierto);
        let (accion, _, motivo) = filas.get(i).ok_or_else(String::new)?;
        match motivo {
            None => Ok(accion.clone()),
            Some(k) => Err((*k).to_owned()),
        }
    }

    /// Mete el catálogo de plugins en el modelo (H3e).
    /// Mete el catálogo de plugins en el modelo (H3e).
    ///
    /// El ÚNICO punto de entrada de texto de tercero a la ayuda de esta
    /// ventana, y hace tres cosas que el modelo no hace:
    ///
    /// 1. **Descarta** —nunca reescribe— un id que no sea reverse-DNS válido.
    ///    Un id es una CLAVE: entra en un `TopicId`, sale como argumento de
    ///    `plugin.help` y es lo que el filtro de la lateral pliega en cada
    ///    tecla. Enmascararlo no es una medida de seguridad, porque no es
    ///    inyectivo: mapearía dos plugins distintos a la misma fila.
    /// 2. **Enmascara y acota** el nombre y el publicador, que son prosa de
    ///    tercero, en la entrada y no al pintar: `PluginNode` documenta su
    ///    título como «ya seguro» y el modelo no enmascara nada.
    /// 3. Cae al **id** cuando el nombre queda en blanco. `name` es
    ///    obligatorio en el manifiesto pero nadie comprueba que diga algo, y
    ///    un nombre de rellenos HANGUL pinta una fila vacía bajo la cabecera
    ///    de extensiones: una página que se puede abrir y leer, sin nombre.
    ///
    /// Un nodo está ACTIVO si el plugin está aprobado Y encendido. Eso decide
    /// si sus filas de comando se pueden correr, jamás si su página se ve: un
    /// humano lee la documentación de una extensión precisamente para decidir
    /// si la enciende.
    pub(crate) fn set_plugins(&mut self, plugins: &[norte_proto::methods::PluginInfo]) {
        let validos: Vec<&norte_proto::methods::PluginInfo> = plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .collect();
        self.publicadores = validos
            .iter()
            .filter_map(|p| {
                let quien = plugin_label(&p.publisher);
                (!norte_help::is_blank_id(&quien)).then(|| (p.id.clone(), quien))
            })
            .collect();
        self.estado.set_plugins(
            validos
                .iter()
                .map(|p| {
                    let nombre = plugin_label(&p.name);
                    PluginNode {
                        id: p.id.clone(),
                        title: if norte_help::is_blank_id(&nombre) {
                            plugin_label(&p.id)
                        } else {
                            nombre
                        },
                        has_help: p.has_help,
                        active: p.approved && p.enabled,
                    }
                })
                .collect(),
        );
    }

    /// La página de plugin que hay que pedir, si hay alguna y no se ha pedido
    /// ya en esta apertura. Reclamarla la marca como pedida.
    pub(crate) fn reclamar_pagina(&mut self) -> Option<String> {
        let id = self.estado.plugin_needs_fetch()?.to_owned();
        self.pedidas.insert(id.clone()).then_some(id)
    }

    /// Instala la página de un plugin, parseada como lo que es: texto que no
    /// se controla.
    ///
    /// `fold_flags` no es opcional: el texto llega YA acotado y YA decodificado
    /// por el daemon, así que este parseo sale limpio y la insignia —que es
    /// toda la mitigación visible de un `help.md` hostil— se apagaría.
    pub(crate) fn instalar_pagina(
        &mut self,
        id: &str,
        res: &norte_proto::methods::PluginHelpResult,
    ) {
        // El publicador sale de la FOTO, nunca de la página: un plugin no
        // dice quién lo publica. `parse_untrusted` lo vuelve a enmascarar,
        // que es inofensivo.
        let publicador = self.publicadores.get(id).cloned();
        let parsed = norte_help::parse_untrusted(res.markdown.as_bytes(), id, publicador)
            .fold_flags(res.truncated, res.lossy);
        self.estado.install_plugin_topic(parsed.topic);
    }

    /// Mueve el cursor del cuerpo a la fila `i` — lo que significa un click
    /// sobre ella, la mitad que NO ejecuta.
    ///
    /// Se hace además de ejecutar, y no en vez de: tras seguir un enlace, la
    /// siguiente flecha tiene que moverse por donde el lector acaba de
    /// señalar, no por la lateral.
    pub(crate) fn senalar(&mut self, i: usize) {
        self.estado.click_action(i);
    }
}

/// Por qué esta ventana NO puede correr una fila, o `None` si sí puede.
///
/// Son DOS preguntas, y las dos apagan la fila:
///
/// - El **catálogo compartido** contesta «¿se puede AHORA?» con los hechos
///   congelados al abrir la ayuda (dentro de un zip no se copia hacia aquí).
/// - La **lista de esta ventana** contesta «¿lo hace esta ventana?», y esa
///   respuesta depende de la PANTALLA a la que pertenece el comando, no de
///   una lista plana: un verbo `dialog.*` lo contesta el propio diálogo con
///   sus botones, y un comando del visor solo significa algo con el visor
///   abierto. Preguntar contra la lista plana daba las dos respuestas mal a
///   la vez — una página de diálogo salía entera apagada «porque esta ventana
///   no lo hace», y las filas del visor salían encendidas con el visor
///   cerrado para luego negarse al pulsarlas.
fn motivo_de(
    row: &norte_help::CommandRow,
    efectos: crate::commands::Efectos,
    visor_abierto: bool,
) -> Option<&'static str> {
    let cmd = row.command.as_str();
    if cmd.starts_with("dialog.") {
        return Some(norte_frontend::availability::reason_key(
            norte_help::Reason::AnsweredByTheOverlay,
        ));
    }
    if crate::commands::IMPLEMENTADOS_VISOR.contains(&cmd) {
        return (!visor_abierto).then_some("reason-viewer-only");
    }
    if !crate::commands::implementados(efectos).contains(&cmd) {
        return Some("keymap-short-not-here");
    }
    row.avail
        .reason()
        .map(norte_frontend::availability::reason_key)
}

/// Una IDENTIDAD que cruza el bridge: entera, o vacía.
///
/// Nunca recortada. Recortar no es inyectivo, y esto es una clave: dos ids
/// que coincidieran en sus primeros miles de bytes llegarían como uno solo,
/// que es la trampa que ADR 0061 decidió no volver a tender. Una clave que no
/// cabe se convierte en una que no casa con nada, que es un fallo visible, en
/// vez de una que casa con la equivocada.
fn identidad(id: &str) -> String {
    if id.len() > crate::bridge::MAX_STRING_BYTES {
        return String::new();
    }
    id.to_owned()
}

/// La página sintética de teclado: cada tecla ligada de cada pantalla, con su
/// etiqueta del catálogo, en el orden de precedencia REAL del mapa.
///
/// Generada, jamás una lista mantenida a mano: un rebind la cambia. No es una
/// tabla del corpus porque sus filas llevan disponibilidad y motivo, que una
/// celda de tabla no tiene dónde poner.
fn hoja_de_teclado(listado: &Effective, visor: &Effective, lang: Lang) -> Vec<HelpBlockView> {
    let mut out = vec![HelpBlockView::Paragraph {
        spans: vec![HelpSpanView::Text {
            text: clamp_display(norte_i18n::t_in(lang, "keys-page-note")),
        }],
    }];
    for (titulo, screen, eff) in [
        ("help-section-browse", Screen::Browse, listado),
        ("help-section-viewer", Screen::Viewer, visor),
    ] {
        let filas: Vec<HelpKeyRowView> = sheet(&[(screen, eff.clone())])
            .into_iter()
            .map(|row| HelpKeyRowView {
                chord: clamp_display(row.chord),
                label: clamp_display(norte_frontend::whichkey::command_label(&row.command, lang)),
                enabled: row.avail == Availability::Here,
                reason: clamp_display(short_unavailable_message(row.avail, lang)),
            })
            .collect();
        if filas.is_empty() {
            continue;
        }
        out.push(HelpBlockView::Heading {
            level: 2,
            text: clamp_display(norte_i18n::t_in(lang, titulo)),
        });
        out.push(HelpBlockView::Keys { rows: filas });
    }
    out
}

/// La cabecera de un grupo de la lateral, traducida.
///
/// `t_in` contesta una clave que no tiene con la clave misma, así que un tag
/// sin entrada pintaría `help-group-…` al lector: se detecta ese eco —que ES
/// el fallo— y se cae al tag, que al menos es una palabra.
fn etiqueta_de_grupo(tag: &str, lang: Lang) -> String {
    let id = format!("help-group-{tag}");
    let texto = norte_i18n::t_in(lang, &id);
    if texto == id { tag.to_owned() } else { texto }
}

/// El título de una página, o su id si el corpus de este idioma no la tiene.
fn titulo_de(id: &TopicId, lang: Lang) -> String {
    norte_help::topic(lang, id.as_str()).map_or_else(|| id.as_str().to_owned(), |t| t.title.clone())
}

/// Un bloque del corpus, proyectado.
fn bloque(b: &Block, chords: &Chords) -> HelpBlockView {
    match b {
        Block::Heading { level, text } => HelpBlockView::Heading {
            level: (*level).clamp(1, 3),
            text: clamp_display(text.clone()),
        },
        Block::Paragraph(spans) => HelpBlockView::Paragraph {
            spans: spans.iter().map(|s| fragmento(s, chords)).collect(),
        },
        Block::Bullets(items) => HelpBlockView::Bullets {
            items: items
                .iter()
                .map(|spans| spans.iter().map(|s| fragmento(s, chords)).collect())
                .collect(),
        },
        Block::Code { lang, text } => HelpBlockView::Code {
            lang: lang.clone().map(clamp_display),
            text: clamp_display(text.clone()),
        },
        Block::Table { header, rows } => HelpBlockView::Table {
            header: header.iter().cloned().map(clamp_display).collect(),
            rows: rows
                .iter()
                .map(|r| r.iter().cloned().map(clamp_display).collect())
                .collect(),
        },
        Block::Callout { kind, spans } => HelpBlockView::Callout {
            kind: match kind {
                norte_help::Callout::Note => crate::dto::HelpCalloutView::Note,
                norte_help::Callout::Warn => crate::dto::HelpCalloutView::Warn,
                norte_help::Callout::Tip => crate::dto::HelpCalloutView::Tip,
            },
            spans: spans.iter().map(|s| fragmento(s, chords)).collect(),
        },
    }
}

/// Un fragmento, con las dos marcas VIVAS ya resueltas contra el keymap y el
/// idioma de este lector.
fn fragmento(s: &Span, chords: &Chords) -> HelpSpanView {
    match s {
        Span::Text(t) => HelpSpanView::Text {
            text: clamp_display(t.clone()),
        },
        Span::Strong(t) => HelpSpanView::Strong {
            text: clamp_display(t.clone()),
        },
        Span::Emph(t) => HelpSpanView::Emph {
            text: clamp_display(t.clone()),
        },
        Span::Code(t) => HelpSpanView::Code {
            text: clamp_display(t.clone()),
        },
        Span::CommandRef(c) => {
            // El último escalón de `render_command` es el ID CRUDO del
            // comando, y en modo confiable el parser no comprueba su
            // alfabeto: `{{cmd:\u{202E}fs.copy}}` sobrevive intacto. Lo que
            // hace seguro no enmascarar aquí es que el corpus EMBEBIDO pasa
            // por un gate (`check_commands` sobre la lista compartida, que
            // hoy corre en `norte-tui/tests/help_gate.rs`) y este host pinta
            // ESE mismo corpus. Una página de plugin no cuenta: viene de
            // `parse_untrusted`, que rehúsa una clave que no podría pintar.
            let texto = norte_help::render_command(c, chords);
            HelpSpanView::Command {
                is_chord: texto.is_chord(),
                text: clamp_display(texto.into_text()),
            }
        }
        Span::TopicLink(id) => HelpSpanView::Link {
            text: clamp_display(titulo_de(id, chords_lang(chords))),
        },
    }
}

/// El idioma con el que se construyó el resolver.
///
/// Se pregunta al resolver y no se pasa como parámetro porque son el mismo
/// idioma por construcción, y dos fuentes es donde una página acaba con el
/// título en un idioma y la prosa en otro.
fn chords_lang(chords: &Chords) -> Lang {
    chords.lang()
}
