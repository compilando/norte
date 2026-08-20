//! El listado remoto, página a página.
//!
//! `fs.list` responde una página y un cursor; el stream que ve el llamante
//! ([`super::RemoteBackend::list_stream`]) va pidiendo la siguiente a medida
//! que se consume, para que un directorio de medio millón de entradas no
//! viaje en un frame ni espere a estar entero para pintar la primera fila.

use norte_proto::methods::{FsListParams, FsListResult};
use norte_proto::{Entry, Error, VPath, methods};

use super::RemoteBackend;

/// Entradas por página al listar un dir remoto (ADR 0017): acota el frame
/// de respuesta y el tiempo de UNA llamada.
pub(super) const LIST_PAGE: u32 = 1000;

/// Estado del `try_unfold` que pagina un listado remoto: el buffer de la
/// página actual y el cursor de la siguiente.
pub(super) struct PageState {
    pub(super) backend: RemoteBackend,
    pub(super) dir: VPath,
    pub(super) buffer: std::collections::VecDeque<Entry>,
    pub(super) cursor: Option<String>,
    pub(super) done: bool,
    /// Ids de attrs del ARRANQUE (#108 bloque 2): el daemon ignora los de
    /// una continuación (el stream retenido nació con ellos), pero se
    /// re-mandan igual — si el cursor expira y el cliente reinicia, el
    /// nuevo listado pide lo mismo.
    pub(super) attrs: Vec<String>,
}

/// Un paso del stream paginado: sirve del buffer o pide la página siguiente.
pub(super) async fn page_step(mut st: PageState) -> Result<Option<(Entry, PageState)>, Error> {
    loop {
        if let Some(e) = st.buffer.pop_front() {
            return Ok(Some((e, st)));
        }
        if st.done {
            return Ok(None);
        }
        let cursor = st.cursor.take();
        let page: FsListResult = st
            .backend
            .call_timed_guarded(
                methods::FS_LIST,
                &FsListParams {
                    path: st.dir.clone(),
                    limit: Some(LIST_PAGE),
                    cursor,
                    attrs: st.attrs.clone(),
                },
            )
            .await?;
        // Un server roto que devuelve página vacía CON next_cursor haría
        // un bucle infinito: se corta (precedente del guard de fs.read).
        if page.entries.is_empty() && page.next_cursor.is_some() {
            return Err(Error::Internal { panic: false });
        }
        st.buffer.extend(page.entries);
        match page.next_cursor {
            Some(c) => st.cursor = Some(c),
            None => st.done = true,
        }
    }
}
