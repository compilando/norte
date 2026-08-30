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

/// Las anclas de los directorios que un cliente ha LISTADO, con tope y orden
/// de llegada (#301).
///
/// El gemelo de la que `norte-client` guarda en su `Inner` para el camino
/// remoto, y existe por lo mismo: quien lista es el panel y quien escribe
/// después puede ser otro clon del mismo backend, así que la memoria vive
/// junto al estado compartido y no en el frontend. En el camino EMBEBIDO ese
/// estado compartido es el [`Engine`](crate::Engine), que es lo único que un
/// `Backend::Embedded` clonado comparte.
///
/// Acotado y best-effort: son directorios que un humano tiene abiertos, o sea
/// unidades. Perder uno cuesta la comprobación de esa escritura, jamás la
/// escritura.
///
/// **Duplicada a propósito y no compartida con el SDK**: exportarla desde
/// `norte-client` ataría el camino embebido —que existe para funcionar SIN
/// daemon, y compila en plataformas donde el transporte del SDK no— a un
/// crate que no necesita para nada. Son cuarenta líneas y un tope.
#[derive(Debug, Default)]
pub(crate) struct AnchorCache {
    by_dir: std::collections::HashMap<norte_proto::VPath, DirAnchor>,
    order: std::collections::VecDeque<norte_proto::VPath>,
}

/// Cuántos directorios se recuerdan a la vez. El mismo número que el SDK.
const ANCHORS_MAX: usize = 64;

impl AnchorCache {
    /// Recuerda (o refresca) el ancla de `dir`.
    ///
    /// `None` BORRA la que hubiera, y eso es deliberado: un listado que ya no
    /// trae ancla —porque el provider dejó de saber darla— no puede dejar viva
    /// la de antes. Una escritura que mandara un ancla vieja se rechazaría a sí
    /// misma sin motivo.
    ///
    /// El desalojo es **LRU y no FIFO**: refrescar mueve el directorio al final
    /// de la cola. Con FIFO —que es lo que hacía, y lo que sigue haciendo el
    /// SDK— el directorio del panel activo se desalojaba en cuanto pasaban 64
    /// directorios DISTINTOS por el mismo backend, por mucho que se estuviera
    /// relistando cada segundo; y entonces la comprobación de la siguiente
    /// escritura desaparecía sin que nadie lo dijera, que es fallar abierto en
    /// silencio.
    pub(crate) fn remember(&mut self, dir: &norte_proto::VPath, anchor: Option<DirAnchor>) {
        let Some(anchor) = anchor else {
            self.by_dir.remove(dir);
            self.order.retain(|d| d != dir);
            return;
        };
        if self.by_dir.insert(dir.clone(), anchor).is_some() {
            self.order.retain(|d| d != dir);
        }
        self.order.push_back(dir.clone());
        while self.order.len() > ANCHORS_MAX {
            if let Some(viejo) = self.order.pop_front() {
                self.by_dir.remove(&viejo);
            }
        }
    }

    /// El ancla retenida de `dir`, si se listó.
    pub(crate) fn get(&self, dir: &norte_proto::VPath) -> Option<DirAnchor> {
        self.by_dir.get(dir).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vpd(wire: &str) -> norte_proto::VPath {
        norte_proto::VPath::parse(wire).expect("wire de test")
    }

    #[test]
    fn se_recuerda_lo_listado_y_solo_eso() {
        let mut c = AnchorCache::default();
        let dir = vpd("file:///casa");
        let a = DirAnchor::new("a".repeat(32));
        c.remember(&dir, Some(a.clone()));
        assert_eq!(c.get(&dir), Some(a));
        assert_eq!(c.get(&vpd("file:///otro")), None);
    }

    /// Un listado SIN ancla borra la de antes: mandar la vieja sería que la
    /// escritura se rechazara a sí misma.
    #[test]
    fn un_listado_sin_ancla_borra_la_de_antes() {
        let mut c = AnchorCache::default();
        let dir = vpd("file:///casa");
        c.remember(&dir, Some(DirAnchor::new("b".repeat(32))));
        c.remember(&dir, None);
        assert_eq!(c.get(&dir), None);
    }

    /// El tope echa al que hace más que no se toca.
    #[test]
    fn el_tope_echa_al_mas_viejo() {
        let mut c = AnchorCache::default();
        for i in 0..=ANCHORS_MAX {
            c.remember(
                &vpd(&format!("file:///d{i}")),
                Some(DirAnchor::new(format!("{i:032x}"))),
            );
        }
        assert_eq!(c.get(&vpd("file:///d0")), None, "el primero se fue");
        assert!(c.get(&vpd(&format!("file:///d{ANCHORS_MAX}"))).is_some());
    }

    /// Y refrescar lo SALVA: es LRU y no FIFO. Con FIFO, el directorio del
    /// panel activo se desalojaba a los 64 directorios distintos aunque se
    /// estuviera relistando todo el rato, y la comprobación de la siguiente
    /// escritura desaparecía sin decir nada.
    #[test]
    fn refrescar_salva_del_desalojo() {
        let mut c = AnchorCache::default();
        let panel = vpd("file:///panel");
        c.remember(&panel, Some(DirAnchor::new("a".repeat(32))));
        for i in 0..ANCHORS_MAX {
            // Cada vuelta relista el panel, como hace un refresco de verdad.
            c.remember(&panel, Some(DirAnchor::new("a".repeat(32))));
            c.remember(
                &vpd(&format!("file:///d{i}")),
                Some(DirAnchor::new(format!("{i:032x}"))),
            );
        }
        assert!(
            c.get(&panel).is_some(),
            "lo que se sigue mirando no se desaloja"
        );
    }

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
