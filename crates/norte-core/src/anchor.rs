//! El ancla de un directorio: qué nodo estaba mirando el humano (#295).
//!
//! Un [`DirAnchor`] es lo que un listado devuelve y lo que la copia o el
//! movimiento devuelven para decir «el directorio destino era ESE». Sirve para
//! lo único que ADR 0072 deja abierto: un enlace **ya plantado** cuando el core
//! mira por primera vez es, desde dentro del core, indistinguible de un
//! `~/copias -> /mnt/disco/copias` legítimo. Desde fuera sí hay algo que los
//! distingue — el humano no estaba mirando ese otro nodo.
//!
//! # Por qué es opaco
//!
//! Lo que identifica un nodo es un par (volumen, índice), o sea el dispositivo
//! y el inodo. Mandarlos crudos por el wire diría a cualquier cliente —un
//! agente con scope, un plugin— qué dos rutas son el mismo fichero y qué
//! números de inodo existen, que no es asunto suyo. Así que lo que viaja es
//! `sha256(secreto || volumen || índice)` recortado a 128 bits: la igualdad se
//! conserva, que es lo único que se necesita, y el nodo no se puede deducir ni
//! el ancla fabricar.
//!
//! El secreto se sortea UNA vez por proceso. Un daemon que reinicia renueva el
//! secreto y con él todas las anclas, pero un cliente que reconecta ha perdido
//! su listado de todas formas y vuelve a pedirlo: la ventana que importa
//! —mirar, aprobar, escribir— cae entera dentro de una sesión.

use norte_proto::DirAnchor;
use norte_vfs::NodeId;

/// El secreto del proceso. Se sortea en el primer uso.
static SECRETO: std::sync::OnceLock<[u8; 32]> = std::sync::OnceLock::new();

fn secreto() -> &'static [u8; 32] {
    SECRETO.get_or_init(|| {
        let mut bytes = [0u8; 32];
        // Un fallo del CSPRNG del sistema no puede degradar a un secreto
        // predecible: sin secreto de verdad, un cliente podría fabricar el
        // ancla de un nodo que no ha visto y la comprobación dejaría de
        // comprobar. `getrandom` solo falla si el sistema no tiene entropía,
        // que aquí es tan fatal como no tener sistema de ficheros.
        getrandom::fill(&mut bytes).expect("el sistema no da entropía para el secreto de anclas");
        bytes
    })
}

/// El ancla de `id`: la misma para el mismo nodo mientras viva el proceso,
/// distinta para nodos distintos, y sin nada dentro que lo delate.
#[must_use]
pub fn de_nodo(id: NodeId) -> DirAnchor {
    use sha2::{Digest as _, Sha256};
    let mut h = Sha256::new();
    h.update(secreto());
    h.update(id.volume.to_le_bytes());
    h.update(id.index.to_le_bytes());
    let d = h.finalize();
    let mut hex = String::with_capacity(norte_proto::DIR_ANCHOR_LEN);
    for b in &d[..norte_proto::DIR_ANCHOR_LEN / 2] {
        use std::fmt::Write as _;
        let _ = write!(hex, "{b:02x}");
    }
    DirAnchor::new(hex)
}

/// ¿El ancla que trae la petición nombra al nodo `id`?
///
/// Un ancla mal formada no casa con nada: no hace falta un caso aparte para
/// ella, porque [`de_nodo`] jamás produce una, y fallar cerrado es lo correcto
/// —el ancla existe para autorizar, no para dispensar—.
#[must_use]
pub fn casa(esperada: &DirAnchor, id: NodeId) -> bool {
    de_nodo(id) == *esperada
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn el_mismo_nodo_da_la_misma_ancla_y_otro_nodo_no() {
        let a = NodeId {
            volume: 7,
            index: 42,
        };
        let b = NodeId {
            volume: 7,
            index: 43,
        };
        assert_eq!(de_nodo(a), de_nodo(a));
        assert_ne!(de_nodo(a), de_nodo(b));
        assert!(casa(&de_nodo(a), a));
        assert!(!casa(&de_nodo(a), b));
    }

    #[test]
    fn el_ancla_no_lleva_dentro_el_inodo_ni_el_volumen() {
        // El caso que hace la prueba interesante: dos nodos que solo se
        // diferencian en el volumen. Si el ancla llevara los números, uno
        // sería prefijo o vecino del otro.
        let a = NodeId {
            volume: 1,
            index: 999_999,
        };
        let b = NodeId {
            volume: 2,
            index: 999_999,
        };
        let (x, y) = (de_nodo(a), de_nodo(b));
        assert_ne!(x, y);
        assert!(!x.as_str().contains("999999"), "no lleva el índice dentro");
        assert!(x.is_well_formed() && y.is_well_formed());
    }

    #[test]
    fn un_ancla_mal_formada_no_casa_con_nada() {
        let id = NodeId {
            volume: 3,
            index: 3,
        };
        assert!(!casa(&DirAnchor::new(String::new()), id));
        assert!(!casa(&DirAnchor::new("../etc".to_owned()), id));
        // Y tampoco la que alguien fabricaría sin el secreto: el hash del par
        // a pelo, que es lo que se le ocurriría a quien conozca el formato.
        let sin_secreto = {
            use sha2::{Digest as _, Sha256};
            let mut h = Sha256::new();
            h.update(id.volume.to_le_bytes());
            h.update(id.index.to_le_bytes());
            let d = h.finalize();
            let mut hex = String::new();
            for b in &d[..16] {
                use std::fmt::Write as _;
                let _ = write!(hex, "{b:02x}");
            }
            DirAnchor::new(hex)
        };
        assert!(!casa(&sin_secreto, id), "sin el secreto no se fabrica");
    }
}
