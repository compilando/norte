//! El área de sesión de UI de [`Backend`](super::Backend) (L2): leer,
//! reemplazar y soltar la sesión guardada.

use norte_proto::Error;

use super::Backend;

impl Backend {
    /// La sesión de UI guardada y si ESTA superficie puede escribirla (L2).
    ///
    /// Contra el daemon es `session.get`. En EMBEBIDO no hay socket, así que
    /// el proceso es su propio almacén: el fichero de `<state_dir>` y el mismo
    /// lock que usa el daemon, tomado una vez por proceso. Sin `state_dir` —un
    /// entorno sin `HOME`— se sirve una sesión vacía que nadie escribe, que es
    /// exactamente lo que hoy hace un arranque sin sesión guardada.
    ///
    /// # Errors
    ///
    /// Lo que devuelva el transporte. Un fallo NO es motivo para no arrancar:
    /// el llamante sigue con la pantalla de la configuración.
    pub async fn session_get(&self) -> Result<(norte_proto::methods::Session, bool), Error> {
        match self {
            Self::Embedded(_) => Ok(crate::embedded::session_get().await),
            #[cfg(unix)]
            Self::Remote(r) => r.session_get().await,
        }
    }

    /// Reemplaza la sesión de UI y devuelve la revisión NUEVA (L2).
    ///
    /// # Errors
    ///
    /// [`Error::Conflict`] si la revisión venía rancia (re-lee y reintenta),
    /// [`Error::LimitExceeded`] si el cuerpo pasa del tope, y lo que dé el
    /// transporte en lo demás.
    pub async fn session_put(
        &self,
        version: u32,
        revision: u64,
        body: serde_json::Value,
    ) -> Result<u64, Error> {
        match self {
            Self::Embedded(_) => crate::embedded::session_put(version, revision, body).await,
            #[cfg(unix)]
            Self::Remote(r) => r.session_put(version, revision, body).await,
        }
    }

    /// Suelta la propiedad de la sesión de UI (0.78.0, fase 9). Devuelve si
    /// esta conexión ERA la dueña.
    ///
    /// **En EMBEBIDO no hay a quién soltársela**: el proceso es el único que
    /// toca esa sesión, así que contesta `false` sin tocar nada. No es una
    /// degradación silenciosa — es la razón por la que `app.handoff` se
    /// declara no disponible fuera del modo daemon, con su motivo.
    ///
    /// # Errors
    /// Lo que dé el transporte. Un daemon 0.77 contesta `Unsupported`.
    pub async fn session_release(&self) -> Result<bool, Error> {
        match self {
            Self::Embedded(_) => Ok(false),
            #[cfg(unix)]
            Self::Remote(r) => r.session_release().await,
        }
    }
}
