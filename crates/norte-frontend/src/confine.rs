//! ¿Puede el destino sujetar sus propias escrituras? (#164, ADR 0054)
//!
//! Un `Copy` recursivo compone `destino + relativo` paso a paso, y un symlink
//! puesto en un componente INTERMEDIO entre que el humano dice que sí y que los
//! bytes se escriben manda la copia a otro sitio. Donde el sistema sabe abrir
//! relativo a un descriptor —Linux y macOS— el core abre la raíz una vez y ese
//! desvío deja de existir. Donde no —Windows, SFTP, un bucket—, la copia se
//! hace igual, por ruta, como se ha hecho siempre.
//!
//! Aquí se AVISA de eso segundo, y no se rehúsa. El mismo contrato que
//! [`crate::space`]: se pone el hecho delante y decide el humano. Rehusar
//! dejaría sin copiar a los destinos que no pueden dar esa defensa, que es un
//! precio muchísimo más alto que la carrera que evita — y esa carrera pide que
//! alguien con acceso al árbol de destino plante un symlink en el momento
//! exacto.
//!
//! # Lo que la línea NO promete
//!
//! Su ausencia dice que el destino SABE confinar, no que esta operación en
//! concreto vaya confinada: una hoja suelta no cuelga de ninguna raíz aprobada
//! —no hay ventana que aprovechar, la ruta se compone y se escribe seguido— y
//! el core no le abre raíz. Lo que la capability describe es la UBICACIÓN, que
//! es de lo que va ADR 0054.

use norte_i18n::{Lang, t_in};
use norte_proto::{Capabilities, CapabilityFlags};

/// El aviso, o `None` cuando el destino sabe confinar.
///
/// Que sepa NO se anuncia: una línea en cada copia es ruido, y el ruido enseña
/// a saltarse la línea justo el día que dice algo.
///
/// ```
/// use norte_frontend::confine::warning;
/// use norte_i18n::Lang;
/// use norte_proto::{Capabilities, CapabilityFlags};
///
/// let confina = Capabilities {
///     flags: CapabilityFlags::CONFINED_WRITES,
///     max_path: None,
/// };
/// assert!(warning(confina, Lang::En).is_none());
///
/// let no = Capabilities { flags: CapabilityFlags::empty(), max_path: None };
/// assert!(warning(no, Lang::En).is_some(), "el humano decide, pero enterado");
/// ```
#[must_use]
pub fn warning(caps: Capabilities, lang: Lang) -> Option<String> {
    if caps.flags.contains(CapabilityFlags::CONFINED_WRITES) {
        return None;
    }
    Some(t_in(lang, "confine-warning"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps(flags: CapabilityFlags) -> Capabilities {
        Capabilities {
            flags,
            max_path: None,
        }
    }

    /// Un destino que confina no dice nada.
    #[test]
    fn un_destino_que_confina_se_calla() {
        assert_eq!(
            warning(caps(CapabilityFlags::CONFINED_WRITES), Lang::En),
            None
        );
    }

    /// Y uno que no, lo dice — sin bloquear nada.
    #[test]
    fn un_destino_que_no_puede_confinar_avisa() {
        let aviso = warning(caps(CapabilityFlags::empty()), Lang::En)
            .expect("el humano decide, pero enterado");
        assert!(!aviso.is_empty(), "la clave existe en el catálogo");
    }

    /// El resto de flags no tiene voz en esto: lo que se mira es UNO, y un
    /// destino cargado de capacidades que no incluyan esta avisa igual.
    #[test]
    fn ningun_otro_flag_lo_silencia() {
        let ruidoso = CapabilityFlags::all() - CapabilityFlags::CONFINED_WRITES;
        assert!(warning(caps(ruidoso), Lang::En).is_some());
    }

    /// Y la línea está en los dos idiomas: una clave ausente saldría como el
    /// nombre de la clave, que es peor que no avisar.
    #[test]
    fn la_linea_existe_en_los_dos_idiomas() {
        let en = warning(caps(CapabilityFlags::empty()), Lang::En).expect("en");
        let es = warning(caps(CapabilityFlags::empty()), Lang::Es).expect("es");
        assert_ne!(en, "confine-warning", "clave sin traducir en inglés");
        assert_ne!(es, "confine-warning", "clave sin traducir en español");
        assert_ne!(en, es, "y no son la misma frase");
    }
}
