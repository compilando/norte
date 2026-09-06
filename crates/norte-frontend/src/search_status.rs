//! En qué acabó una búsqueda, y qué frase le corresponde.
//!
//! Los desenlaces se dicen distinto porque significan cosas distintas **sobre
//! el disco**, no sobre la interfaz: «no hay más» es una respuesta, «la paré»
//! es media respuesta y «se rompió» no es ninguna. Colapsarlos deja a la
//! pantalla afirmando que un directorio no contiene lo que se buscaba cuando
//! lo que pasó es que nadie llegó a mirar.
//!
//! Vive aquí porque los dos frontends lo decidían por su cuenta y **con
//! precedencias distintas** (ADR 0077): una búsqueda que el lector paraba
//! justo en el tope decía «cancelada» en el terminal y «hay más» en la
//! ventana. Ahora la precedencia es una, y cambiarla es cambiar los dos.

use norte_proto::TaskState;

/// En qué acabó, o que sigue.
///
/// No es `TaskState`: eso es del wire y tiene estados que a una búsqueda no
/// le dicen nada (`Pending`, `Paused`, `Unknown`). Esto es lo que hay que
/// contarle al lector.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Sigue corriendo, o todavía no ha empezado. Las dos cosas son «espera».
    Running,
    /// Terminó de recorrer lo que había.
    Done,
    /// La paró el lector. Lo encontrado vale; lo que falta no se llegó a
    /// mirar.
    Cancelled,
    /// Se rompió, y con qué CATEGORÍA de error (ya traducida).
    ///
    /// Persistente: un fallo jamás degrada a «hecha» en la siguiente tecla.
    /// Eso costó una revisión en el terminal y aquí se hereda escrito.
    ///
    /// Guarda el texto y no el `Error` porque `ui.lang` NO cambia en caliente
    /// (`norte_i18n::force` corre una vez por proceso; la lista de claves
    /// fuera de alcance en caliente del host lo dice por su nombre). El día
    /// que el idioma se pueda cambiar sin reiniciar, esto tiene que pasar a
    /// guardar el error y traducir al pintar.
    Failed(String),
}

/// Qué le pasó a una task de búsqueda, si es que le pasó algo.
///
/// `None` = no es un desenlace: `Running` y `Pending` son «espera», y
/// `Unknown` —un estado de un protocolo más nuevo— también, porque
/// [`TaskState::is_terminal`] lo cuenta como no-terminal a propósito: ante
/// algo que no entiende, el cliente sigue escuchando.
///
/// Un `_ => Done` en vez de esto es lo que hacía que una búsqueda encolada o
/// pausada se anunciara como terminada sin hallazgos.
///
/// ```
/// use norte_frontend::search_status::{Outcome, outcome_of};
/// use norte_proto::TaskState;
///
/// assert_eq!(outcome_of(&TaskState::Completed, |_| unreachable!()), Some(Outcome::Done));
/// assert_eq!(outcome_of(&TaskState::Cancelled, |_| unreachable!()), Some(Outcome::Cancelled));
/// // Ni encolada ni pausada son un desenlace.
/// assert_eq!(outcome_of(&TaskState::Pending, |_| unreachable!()), None);
/// assert_eq!(outcome_of(&TaskState::Paused, |_| unreachable!()), None);
/// assert_eq!(outcome_of(&TaskState::Running, |_| unreachable!()), None);
/// ```
#[must_use]
pub fn outcome_of(
    state: &TaskState,
    categoria: impl FnOnce(&norte_proto::Error) -> String,
) -> Option<Outcome> {
    match state {
        TaskState::Completed => Some(Outcome::Done),
        TaskState::Cancelled => Some(Outcome::Cancelled),
        TaskState::Failed { error } => Some(Outcome::Failed(categoria(error))),
        // `Pending`, `Paused`, `Running` y `Unknown`: nada que anunciar. Y
        // `Unknown` es el que importa — un estado de un protocolo más nuevo
        // NO es terminal (`TaskState::is_terminal` lo excluye a propósito:
        // ante algo que no entiende, el cliente sigue escuchando), así que
        // caer aquí es lo correcto y no un descuido. El wildcard hace falta
        // porque `TaskState` es `#[non_exhaustive]`.
        _ => None,
    }
}

/// La clave Fluent de la frase de estado.
///
/// **La precedencia es la decisión**, y es la del terminal
/// (`norte_tui::jobs::search::finalize_search_state`): cancelada gana a
/// truncada. Las dos dicen «esto no es todo», pero solo una dice POR QUÉ, y
/// «hay más» sobre una búsqueda que el lector paró afirma además que el
/// recorrido llegó a llenar el tope — que después de una cancelación es
/// justo lo que no se sabe.
///
/// `at_cap` solo cuenta sobre una que TERMINÓ: mientras corre, el tope
/// alcanzado no es un desenlace, y anunciarlo como tal ponía una frase
/// terminal al lado de un spinner.
///
/// ```
/// use norte_frontend::search_status::{Outcome, status_key};
///
/// assert_eq!(status_key(&Outcome::Running, false), "search-status-running");
/// assert_eq!(status_key(&Outcome::Running, true), "search-status-running");
/// assert_eq!(status_key(&Outcome::Done, true), "search-status-truncated");
/// assert_eq!(status_key(&Outcome::Done, false), "search-status-done");
/// // Cancelada gana al tope: solo ella dice por qué falta lo que falta.
/// assert_eq!(status_key(&Outcome::Cancelled, true), "search-status-cancelled");
/// assert_eq!(
///     status_key(&Outcome::Failed("permiso".to_owned()), true),
///     "search-status-failed",
/// );
/// ```
#[must_use]
pub fn status_key(outcome: &Outcome, at_cap: bool) -> &'static str {
    match outcome {
        Outcome::Cancelled => "search-status-cancelled",
        Outcome::Failed(_) => "search-status-failed",
        Outcome::Done if at_cap => "search-status-truncated",
        Outcome::Done => "search-status-done",
        Outcome::Running => "search-status-running",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El par que discrepaba entre frontends: parar la búsqueda justo en el
    /// tope. El terminal decía «cancelada» y la ventana «hay más».
    #[test]
    fn cancelada_en_el_tope_dice_cancelada() {
        assert_eq!(
            status_key(&Outcome::Cancelled, true),
            "search-status-cancelled"
        );
    }

    /// Y una VIVA que llega al tope sigue diciendo que corre: una frase
    /// terminal al lado de un spinner se lee como que ya terminó.
    #[test]
    fn viva_en_el_tope_sigue_diciendo_que_corre() {
        assert_eq!(status_key(&Outcome::Running, true), "search-status-running");
    }

    /// Un estado que este cliente no entiende NO es un desenlace: el wire lo
    /// dice (`is_terminal` excluye `Unknown`) y esto tiene que decir lo mismo,
    /// o un daemon más nuevo hace que la ventana anuncie búsquedas terminadas
    /// que siguen corriendo.
    #[test]
    fn un_estado_desconocido_no_termina_nada() {
        assert_eq!(outcome_of(&TaskState::Unknown, |_| String::new()), None);
        for s in [TaskState::Pending, TaskState::Paused, TaskState::Running] {
            assert_eq!(outcome_of(&s, |_| String::new()), None, "{s:?}");
        }
    }

    /// Y los tres que sí lo son se distinguen, con la causa dentro del fallo.
    #[test]
    fn los_tres_desenlaces_se_distinguen() {
        assert_eq!(
            outcome_of(&TaskState::Completed, |_| String::new()),
            Some(Outcome::Done)
        );
        assert_eq!(
            outcome_of(&TaskState::Cancelled, |_| String::new()),
            Some(Outcome::Cancelled)
        );
        assert_eq!(
            outcome_of(
                &TaskState::Failed {
                    error: norte_proto::Error::PermissionDenied
                },
                |_| "sin permiso".to_owned()
            ),
            Some(Outcome::Failed("sin permiso".to_owned()))
        );
    }
}
