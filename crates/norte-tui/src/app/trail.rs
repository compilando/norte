//! El rastro de navegación: qué pasos se registran y cómo se rebobinan.

/// Si una navegación se REGISTRA en el rastro del pane, o es el rastro
/// reproduciéndose a sí mismo.
///
/// Sin esta distinción `nav.back` se alimenta de su propio rastro: volver de
/// B a A registraría "estuve en B", así que el siguiente back devuelve a B y
/// el lector oscila entre dos directorios — el defecto exacto que el rastro
/// existe para evitar, un nivel más arriba.
///
/// Vive aquí (y no junto al `cd` del binario) porque [`crate::app::Modal::TrustHostKey`]
/// lo TRANSPORTA: el reintento tras confiar en la host key debe reanudar la
/// MISMA navegación que el TOFU interrumpió, y la lib no puede referirse a
/// un tipo declarado en `main.rs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trail {
    /// El usuario pidió este movimiento: entra en la MRU y en el rastro, y
    /// poda la rama de forward.
    Record,
    /// `nav.back`/`nav.forward` están reproduciendo, y ESTE es el paso que
    /// están dando. El rastro ya lo sabe, así que la navegación no se
    /// registra; el paso viaja dentro porque un `Replay` sin saber en qué
    /// sentido va no se puede deshacer, y quien tenga que rebobinarlo puede
    /// no ser quien lo empezó: el TOFU suspende la navegación y la respuesta
    /// al modal la termina, minutos después y desde otro sitio del código.
    ///
    /// Va DENTRO de la variante, y no en un campo aparte junto a ella, para
    /// que «registrar» y «tener sentido» no puedan contradecirse: un
    /// `Record` con sentido, o un `Replay` sin él, serían estados que alguien
    /// tendría que acordarse de no construir.
    Replay(TrailStep),
}

impl Trail {
    /// El paso del rastro que esta navegación está dando, si es que está
    /// dando alguno. `None` para un [`Trail::Record`]: no salió del rastro,
    /// así que no hay nada que rebobinar si acaba mal.
    #[must_use]
    pub fn step(self) -> Option<TrailStep> {
        match self {
            Self::Record => None,
            Self::Replay(step) => Some(step),
        }
    }
}

/// Which way `nav.back`/`nav.forward` are walking the trail. The two are the
/// same operation mirrored, so they share one body rather than two arms that
/// must be kept in step by hand.
///
/// Vive aquí por el mismo motivo que [`Trail`], que lo transporta: el modal
/// TOFU ([`crate::app::Modal::TrustHostKey`]) suspende una navegación que puede ser un
/// paso del rastro, y quien responda al modal necesita saber en qué sentido
/// iba para deshacerlo si la respuesta acaba abandonándola.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrailStep {
    /// `nav.back`.
    Back,
    /// `nav.forward`.
    Forward,
}

impl TrailStep {
    /// Fluent id for "there is nothing this way". A key that goes silent is
    /// indistinguishable from a broken one, so the exhausted trail SAYS so.
    #[must_use]
    pub fn empty_message(self) -> &'static str {
        match self {
            Self::Back => "msg-nav-no-back",
            Self::Forward => "msg-nav-no-forward",
        }
    }
}
