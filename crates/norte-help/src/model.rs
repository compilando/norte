//! Modelo del corpus: lo que el parser produce y lo que cada frontend
//! renderiza. Deliberadamente sin nada de UI — ni colores, ni anchos, ni
//! tipos de ratatui/GPUI (regla 7).

use std::fmt;

/// Identificador de un tema (`id` del front matter), único por corpus.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct TopicId(String);

impl TopicId {
    /// Construye un id a partir de cualquier cosa que sea texto.
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// El id como `&str`.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for TopicId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// De dónde sale un tema: del binario o de un plugin de terceros.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Tema del corpus embebido: texto CONFIABLE, jamás enmascarado.
    BuiltIn,
    /// Tema de un `help.md` de plugin: texto de TERCEROS, ya enmascarado y
    /// acotado por el parser (ver `parse_untrusted`).
    Plugin {
        /// Id del plugin en el catálogo.
        id: String,
        /// `publisher` del manifiesto, si lo declara.
        publisher: Option<String>,
        /// El contenido excedió algún tope y se recortó.
        truncated: bool,
        /// El fichero no era UTF-8 válido y se decodificó con pérdida.
        lossy: bool,
    },
}

/// Tipo de aviso de un [`Block::Callout`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Callout {
    /// Nota neutra.
    Note,
    /// Advertencia (algo puede salir mal).
    Warn,
    /// Truco (algo va más rápido).
    Tip,
}

/// Fragmento en línea dentro de un bloque.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Span {
    /// Texto llano.
    Text(String),
    /// Énfasis fuerte (`**así**`).
    Strong(String),
    /// Énfasis (`*así*`).
    Emph(String),
    /// Código en línea (`` `así` ``).
    Code(String),
    /// Referencia a un comando (`{{cmd:fs.copy}}`), SIN resolver: el chord
    /// lo pone el frontend con `ChordResolver` (tarea 9).
    CommandRef(String),
    /// Salto a otro tema (`[[selection]]`), SIN resolver.
    TopicLink(TopicId),
}

/// Bloque de contenido. Vocabulario CERRADO (ADR 0040): que un `help.md`
/// hostil no pueda expresar más que esto es justo lo que lo hace seguro.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
    /// Encabezado de nivel 1..=3.
    Heading {
        /// Nivel, saturado a 1..=3.
        level: u8,
        /// Texto del encabezado.
        text: String,
    },
    /// Párrafo.
    Paragraph(Vec<Span>),
    /// Lista de puntos (un nivel, sin anidar).
    Bullets(Vec<Vec<Span>>),
    /// Bloque de código con lenguaje opcional.
    Code {
        /// Etiqueta de lenguaje de la valla, si la hay.
        lang: Option<String>,
        /// Contenido literal, sin interpretar marcas.
        text: String,
    },
    /// Tabla simple con cabecera.
    Table {
        /// Celdas de la cabecera.
        header: Vec<String>,
        /// Filas, cada una con el mismo número de celdas que la cabecera.
        rows: Vec<Vec<String>>,
    },
    /// Aviso destacado.
    Callout {
        /// Tipo de aviso.
        kind: Callout,
        /// Contenido del aviso.
        spans: Vec<Span>,
    },
}

/// Por qué un comando no puede ejecutarse ahora mismo.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Reason {
    /// El backend del pane activo es de solo lectura (p. ej. dentro de un zip).
    ReadOnlyBackend,
    /// El backend no ofrece esa capacidad.
    Unsupported,
    /// El plugin dueño del comando está desactivado o sin aprobar.
    PluginInactive,
    /// La policy lo niega para el actor actual.
    PolicyDenied,
    /// La conexión está degradada.
    ConnectionDegraded,
}

/// Disponibilidad de una fila de comando en el contexto ACTUAL.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Availability {
    /// Se puede ejecutar ahora.
    Available,
    /// No se puede, con motivo para explicarlo.
    Unavailable {
        /// Motivo mostrado junto a la fila atenuada.
        reason: Reason,
    },
}

impl Availability {
    /// `true` si la fila puede ejecutarse.
    #[must_use]
    pub fn is_available(self) -> bool {
        matches!(self, Self::Available)
    }

    /// El motivo, si la fila está indisponible.
    #[must_use]
    pub fn reason(self) -> Option<Reason> {
        match self {
            Self::Available => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

/// Fila ejecutable de un tema: un comando que el usuario puede lanzar desde
/// la ayuda con la MISMA vía de despacho que la palette.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandRow {
    /// Id del comando (`fs.copy`, `plugin:<id>:<cmd>`).
    pub command: String,
    /// Disponibilidad en el contexto actual (la inyecta el frontend).
    pub avail: Availability,
}

/// Un tema del corpus, ya parseado.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Topic {
    /// Id único.
    pub id: TopicId,
    /// Título mostrado.
    pub title: String,
    /// Etiquetas de agrupación en el índice.
    pub tags: Vec<String>,
    /// Temas relacionados.
    pub see_also: Vec<TopicId>,
    /// Comandos que el tema documenta, en orden de aparición deseada.
    pub commands: Vec<String>,
    /// Contextos de UI que abren ESTE tema con F1.
    pub context: Vec<String>,
    /// Cuerpo.
    pub blocks: Vec<Block>,
    /// Procedencia.
    pub origin: Origin,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topic_id_normaliza_y_muestra() {
        let id = TopicId::new("copying");
        assert_eq!(id.as_str(), "copying");
        assert_eq!(id.to_string(), "copying");
    }

    #[test]
    fn una_fila_indisponible_lleva_su_motivo() {
        let row = CommandRow {
            command: "fs.copy".to_owned(),
            avail: Availability::Unavailable {
                reason: Reason::ReadOnlyBackend,
            },
        };
        assert!(!row.avail.is_available());
        assert_eq!(
            row.avail.reason(),
            Some(Reason::ReadOnlyBackend),
            "la UI necesita el motivo para explicarlo, no solo el hecho"
        );
    }
}
