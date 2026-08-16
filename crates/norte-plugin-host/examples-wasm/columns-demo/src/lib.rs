//! Guest WASM de ejemplo (ADR 0037, G3 plan Task 4/G3b): un columns mínimo.
//!
//! Exporta la interfaz `columns` del world `norte-columns`: `column-values`
//! recibe el LOTE de nombres/paths crudos de la página visible (regla 1:
//! bytes, jamás asumidos UTF-8 — el largo se mide en BYTES, no en chars, así
//! que este guest no necesita decodificar nada) y devuelve, POR CADA
//! entrada en el MISMO orden (contrato posicional 1:1, ADR 0037 tabla de
//! decisión 1), el largo del nombre como texto decimal para la columna
//! `"name-len"`; cualquier otro `id` de columna (que este guest no declara
//! en su manifiesto) responde `none` para toda la página — determinista y
//! defensivo, sin adivinar qué querría decir un id que no le pertenece. El
//! e2e del host verifica el round-trip posicional exacto sin depender de
//! ningún estado externo.
#![no_std]

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

wit_bindgen::generate!({
    world: "norte-columns",
    path: "wit",
    // `host-log`/`host-config` viven en OTRO paquete desde la partición
    // (ADR 0041 decisión 4); wit-bindgen exige decidir explícitamente qué
    // hacer con los imports de fuera del paquete del world.
    generate_all,
});

use exports::norte::plugin::columns::{Guest as ColumnsGuest, LocationRef};
use norte::host::host_log;
use norte::location::location;

struct ColumnsDemo;

/// Único id de columna que este guest declara y sabe valorar (mismo id que
/// su manifiesto de prueba en `plugins_column_values_e2e.rs`).
const NAME_LEN_COLUMN: &str = "name-len";

/// Columna de prueba de la capacidad `location` (ADR 0057): por cada entrada
/// devuelve el tamaño que `stat` reporta bajo el token, o `none` si el host no
/// da ubicación (sin capacidad aprobada, o sin token). Un guest sin ubicación
/// tiene que seguir contestando, no fallar.
const STAT_SIZE_COLUMN: &str = "stat-size";

impl ColumnsGuest for ColumnsDemo {
    fn column_values(
        id: String,
        location: Option<LocationRef>,
        entries: Vec<Vec<u8>>,
    ) -> Vec<Option<String>> {
        host_log::log(&format!(
            "columns-demo: id={id} {} entradas, ubicacion={}",
            entries.len(),
            if location.is_some() { "si" } else { "no" }
        ));
        if id != NAME_LEN_COLUMN && id != STAT_SIZE_COLUMN {
            // Un id que este guest no aporta: `none` para TODA la página,
            // nunca se adivina ni se omite del vector posicional.
            return entries.iter().map(|_| None).collect();
        }
        if id == STAT_SIZE_COLUMN {
            let Some(loc) = location else {
                return entries.iter().map(|_| None).collect();
            };
            return entries
                .iter()
                .map(|raw| {
                    // La entrada visible cuelga del PREFIJO, no de la raíz: la
                    // raíz puede ser un ancestro (marcador de proyecto).
                    let mut rel = loc.prefix.clone();
                    if !rel.is_empty() {
                        rel.push(b'/');
                    }
                    rel.extend_from_slice(raw);
                    rel
                })
                .map(|rel| match location::stat(&loc.token, &rel) {
                    Ok(meta) => Some(meta.size.to_string()),
                    // El host dice que no (sin capacidad, token desconocido):
                    // celda vacía, jamás una traba.
                    Err(why) => {
                        host_log::log(&format!("columns-demo: stat denegado: {why}"));
                        None
                    }
                })
                .collect();
        }
        entries
            .iter()
            .map(|raw| Some(raw.len().to_string()))
            .collect()
    }
}

export!(ColumnsDemo);
