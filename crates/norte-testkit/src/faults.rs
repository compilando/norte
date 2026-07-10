//! Inyección de fallos determinista para [`MemProvider`](crate::MemProvider):
//! el copy engine y la cancelación se testean sin tocar disco ni depender
//! del azar (spec §12). Cada fallo se configura ANTES de la operación y se
//! dispara en un punto exacto (op N, byte N).

use std::sync::Mutex;
use std::time::Duration;

use norte_proto::VPath;

/// Clave interna: los segmentos del `VPath` (bytes crudos).
pub(crate) type SegPath = Vec<Vec<u8>>;

pub(crate) fn seg_path(p: &VPath) -> SegPath {
    p.segments().map(<[u8]>::to_vec).collect()
}

/// Configuración de fallos de un [`MemProvider`](crate::MemProvider).
///
/// Se comparte por `Arc`: los tests guardan el handle y mutan la config
/// mientras el provider está en uso. Todo es determinista — nada de
/// probabilidades.
#[derive(Debug, Default)]
pub struct Faults {
    inner: Mutex<FaultState>,
}

#[derive(Debug, Default)]
struct FaultState {
    latency_per_op: Option<Duration>,
    fail_read_at: Option<(SegPath, usize)>,
    fail_write_at: Option<(SegPath, usize)>,
    /// `Some(n)`: quedan `n` operaciones antes de la desconexión.
    disconnect_after: Option<u64>,
    /// Las próximas `n` operaciones fallan retryable (indisponibilidad
    /// TRANSITORIA); luego el provider se recupera solo.
    unavailable_next: u64,
}

impl Faults {
    /// Latencia fija añadida a cada operación (usa el reloj de tokio:
    /// compatible con `tokio::time::pause`).
    pub fn set_latency_per_op(&self, latency: Option<Duration>) {
        self.lock().latency_per_op = latency;
    }

    /// La lectura de `path` falla con [`Error::Io`](norte_proto::Error::Io)
    /// tras entregar exactamente `byte_n` bytes.
    ///
    /// La clave se compara byte-exacta contra el path pedido, SIN fold de
    /// caja: apunta el fallo al mismo string que usará la operación.
    pub fn fail_read_at(&self, path: &VPath, byte_n: usize) {
        self.lock().fail_read_at = Some((seg_path(path), byte_n));
    }

    /// La escritura sobre `path` falla con [`Error::Io`](norte_proto::Error::Io)
    /// en cuanto el total escrito alcanza `byte_n` bytes.
    ///
    /// Clave byte-exacta, sin fold de caja (ver [`Self::fail_read_at`]).
    pub fn fail_write_at(&self, path: &VPath, byte_n: usize) {
        self.lock().fail_write_at = Some((seg_path(path), byte_n));
    }

    /// Tras `n` operaciones más, TODA operación devuelve
    /// [`Error::ProviderUnavailable`](norte_proto::Error::ProviderUnavailable)
    /// con `retryable: true` (el provider "se desconectó").
    pub fn disconnect_after(&self, n: u64) {
        self.lock().disconnect_after = Some(n);
    }

    /// Las próximas `n` operaciones fallan con
    /// [`Error::ProviderUnavailable`](norte_proto::Error::ProviderUnavailable)
    /// `{retryable: true}` y DESPUÉS el provider se recupera solo — la
    /// contraparte transitoria de [`Self::disconnect_after`], para testear
    /// los reintentos con backoff del engine (ADR 0005).
    pub fn unavailable_for_next(&self, n: u64) {
        self.lock().unavailable_next = n;
    }

    /// Borra toda la configuración de fallos.
    pub fn clear(&self) {
        *self.lock() = FaultState::default();
    }

    /// Puerta de entrada de cada operación: aplica latencia y desconexión.
    /// Devuelve `Err` si el provider ya está "desconectado".
    pub(crate) async fn op_gate(&self) -> Result<(), norte_proto::Error> {
        let latency = {
            let mut st = self.lock();
            if st.unavailable_next > 0 {
                st.unavailable_next -= 1;
                return Err(norte_proto::Error::ProviderUnavailable { retryable: true });
            }
            if let Some(remaining) = st.disconnect_after {
                if remaining == 0 {
                    return Err(norte_proto::Error::ProviderUnavailable { retryable: true });
                }
                st.disconnect_after = Some(remaining - 1);
            }
            st.latency_per_op
        };
        if let Some(d) = latency {
            tokio::time::sleep(d).await;
        }
        Ok(())
    }

    /// Snapshot del fallo de lectura para `path`, si aplica.
    pub(crate) fn read_fault_for(&self, key: &SegPath) -> Option<usize> {
        let st = self.lock();
        match &st.fail_read_at {
            Some((p, n)) if p == key => Some(*n),
            _ => None,
        }
    }

    /// Snapshot del fallo de escritura para `path`, si aplica.
    pub(crate) fn write_fault_for(&self, key: &SegPath) -> Option<usize> {
        let st = self.lock();
        match &st.fail_write_at {
            Some((p, n)) if p == key => Some(*n),
            _ => None,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, FaultState> {
        // Invariante: nadie panica con el lock tomado; envenenamiento imposible.
        self.inner.lock().expect("faults lock sano")
    }
}
