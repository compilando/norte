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

use norte_frontend::help::{Action, Focus, HelpState, KEYS_ID, SidebarRow};
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
}

impl Ayuda {
    /// Abre la ayuda sobre la página del CONTEXTO donde está el lector, o
    /// sobre el índice si ese contexto no tiene página.
    ///
    /// La página del contexto se abre como RAÍZ (`open_as_root`): el lector
    /// no navegó hasta ella, la ayuda lo puso ahí, así que «volver» no puede
    /// llevarle a un índice donde nunca estuvo — y una sola pulsación de
    /// `Esc` tiene que dejar una página que nadie pidió.
    pub(crate) fn abrir(
        lang: Lang,
        contexto: Option<&str>,
        listado: &Effective,
        visor: &Effective,
        facts: norte_frontend::availability::Facts,
    ) -> Self {
        let mut estado = HelpState::new(lang, norte_i18n::t_in(lang, "help-topic-keys"));
        if let Some(t) = contexto.and_then(|c| norte_help::topic_for_context(lang, c)) {
            estado.open_as_root(&t.id);
        }
        Self {
            estado,
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
    /// `implementados` es lo que ESTE host ejecuta: una fila que el catálogo
    /// da por viva pero que este frontend no hace se ofrece apagada y con su
    /// motivo, en vez de prometer un `enter` que contestaría «aquí no».
    pub(crate) fn vista(&self, lang: Lang, implementados: &[&str]) -> HelpView {
        let topic = self.estado.current_topic();
        let en_teclas = self.estado.current().as_str() == KEYS_ID;
        let titulo = if en_teclas {
            norte_i18n::t_in(lang, "help-topic-keys")
        } else {
            topic.map_or_else(String::new, |t| t.title.clone())
        };
        HelpView {
            title: clamp_display(titulo),
            topic_id: clamp_display(self.estado.current().as_str().to_owned()),
            badge: topic.and_then(|t| match &t.origin {
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
            }),
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
            actions: self.acciones(lang, implementados),
            action_cursor: (!self.estado.actions().is_empty())
                .then_some(self.estado.action_cursor() as u64),
            filter: clamp_display(self.estado.filter_display()),
            filtering: self.estado.filtering(),
            can_back: self.estado.can_back(),
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

    /// Lo que `enter` puede hacer, en el MISMO orden que
    /// [`HelpState::actions`]: primero los comandos que la página documenta,
    /// luego sus enlaces de «ver también».
    fn acciones(&self, lang: Lang, implementados: &[&str]) -> Vec<HelpActionView> {
        let Some(topic) = self.estado.current_topic() else {
            return Vec::new();
        };
        let mut out: Vec<HelpActionView> = rows_of(topic, &self.chords)
            .into_iter()
            .map(|r| {
                // DOS preguntas distintas, y las dos apagan la fila. El
                // catálogo contesta «este comando existe y se puede AHORA»;
                // la lista del host contesta «esta ventana lo hace». Sin la
                // segunda, una página ofrecería un `enter` que responde
                // «aquí no» — prometer y luego negarse.
                let aqui = implementados.contains(&r.row.command.as_str());
                let motivo = if aqui {
                    r.row.avail.reason().map_or_else(String::new, |why| {
                        norte_i18n::t_in(lang, norte_frontend::availability::reason_key(why))
                    })
                } else {
                    short_unavailable_message(Availability::NotHere, lang)
                };
                HelpActionView {
                    label: clamp_display(r.label),
                    chord: clamp_display(r.chord.unwrap_or_default()),
                    enabled: aqui && r.row.avail.is_available(),
                    reason: clamp_display(motivo),
                    opens_topic: false,
                }
            })
            .collect();
        out.extend(topic.see_also.iter().map(|id| HelpActionView {
            label: clamp_display(titulo_de(id, self.estado.lang())),
            chord: String::new(),
            // Un enlace siempre se puede seguir: lo único que hace es
            // cambiar de página, y si el corpus de este idioma no la tiene,
            // el propio modelo lo ignora sin panicar.
            enabled: true,
            reason: String::new(),
            opens_topic: true,
        }));
        debug_assert_eq!(
            out.len(),
            self.estado.actions().len(),
            "la proyección y el modelo tienen que enumerar lo mismo"
        );
        out
    }

    /// Qué hace `enter` (o un click) sobre la acción `i`.
    pub(crate) fn accion(&self, i: usize) -> Option<Action> {
        self.estado.actions().get(i).cloned()
    }
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
                norte_help::Callout::Note => "note",
                norte_help::Callout::Warn => "warn",
                norte_help::Callout::Tip => "tip",
            }
            .to_owned(),
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
            let texto = norte_help::render_command(c, chords);
            HelpSpanView::Command {
                is_chord: texto.is_chord(),
                text: clamp_display(texto.into_text()),
            }
        }
        Span::TopicLink(id) => HelpSpanView::Link {
            topic: clamp_display(id.as_str().to_owned()),
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
