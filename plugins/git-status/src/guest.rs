//! La capa WASM: los bindings del world `norte-columns` y nada más.
//!
//! Todo lo que decide algo vive en los otros módulos y se prueba en el host.
//! Aquí solo se traduce: el token y el prefijo que da el host a las llamadas
//! de [`crate::status::Location`], y el veredicto a celdas.

extern crate alloc;

use alloc::string::String;
use alloc::vec::Vec;

use crate::status::{Location, Meta};

wit_bindgen::generate!({
    world: "norte-columns",
    path: "wit",
    generate_all,
});

use exports::norte::plugin::columns::{Guest as ColumnsGuest, LocationRef};
use norte::host::host_log;
use norte::location::location;

/// La ubicación del host, vista como [`Location`].
struct HostLocation {
    token: String,
}

impl Location for HostLocation {
    fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String> {
        location::read(&self.token, rel)
    }

    fn stat(&self, rel: &[u8]) -> Result<Meta, String> {
        let meta = location::stat(&self.token, rel)?;
        Ok(Meta {
            is_dir: matches!(meta.kind, location::EntryKind::Dir),
            size: meta.size,
            mtime_sec: meta.mtime_sec,
            mtime_nsec: meta.mtime_nsec,
        })
    }
}

struct GitStatus;

impl ColumnsGuest for GitStatus {
    fn column_values(
        id: String,
        location: Option<LocationRef>,
        entries: Vec<Vec<u8>>,
    ) -> Vec<Option<String>> {
        if id != crate::COLUMN_ID {
            return entries.iter().map(|_| None).collect();
        }
        // Sin ubicación aprobada no hay nada que decir, y decirlo con celdas
        // vacías es la respuesta correcta: el panel sigue pintándose.
        let Some(loc) = location else {
            return entries.iter().map(|_| None).collect();
        };
        let host = HostLocation {
            token: loc.token.clone(),
        };
        let Ok(raw) = host.read(b".git/index") else {
            // Sin índice no hay repositorio (o el host no llegó a abrirlo):
            // celdas vacías, jamás una marca inventada.
            return entries.iter().map(|_| None).collect();
        };
        let index = match crate::index::GitIndex::parse(&raw) {
            Ok(index) => index,
            Err(why) => {
                host_log::log(&alloc::format!("git-status: índice ilegible: {why:?}"));
                return entries.iter().map(|_| None).collect();
            }
        };
        // El mtime del PROPIO índice es lo que decide si una entrada es
        // «racy»: sin él, un cambio hecho dentro del mismo segundo pasa por
        // limpio.
        let index_mtime = host.stat(b".git/index").map_or(0, |m| m.mtime_sec);
        let ignores = crate::load_ignores(&host, &loc.prefix);
        crate::status::status_for(&index, &ignores, &host, &loc.prefix, &entries, index_mtime)
    }
}

export!(GitStatus);
