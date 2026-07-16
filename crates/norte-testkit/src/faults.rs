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
    /// Las próximas `n` MUTACIONES que se apliquen devuelven error
    /// transitorio DESPUÉS de aplicar su efecto (ambigüedad post-efecto).
    ambiguous_next: u64,
    /// `copy_native` se queda PENDIENTE mientras esté armado (simula un
    /// multipart copy S3 de minutos): solo la cancelación del caller —
    /// dropear el future — lo termina. Determinista, sin latencia global.
    hold_copy_native: bool,
    /// `true` desde que un `copy_native` ENTRÓ en el gate: el test sincroniza
    /// su cancel con esta señal, sin sleeps a ciegas.
    copy_native_entered: bool,
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

    /// Las próximas `n` mutaciones puntuales (`mkdir`/`remove`/`rename`/
    /// `symlink`) que lleguen a APLICARSE devuelven
    /// [`Error::ProviderUnavailable`](norte_proto::Error::ProviderUnavailable)
    /// `{retryable: true}` DESPUÉS de aplicar su efecto — el "timeout tras
    /// commit" de un provider remoto (issue #17): el caller no puede saber
    /// si la mutación ocurrió. Las lecturas y las mutaciones que fallan por
    /// otra causa NO consumen el contador.
    pub fn ambiguous_mutations(&self, n: u64) {
        self.lock().ambiguous_next = n;
    }

    /// Arma (o desarma) el gate de `copy_native`: armado, la copia nativa se
    /// queda PENDIENTE indefinidamente — el equivalente determinista de un
    /// multipart copy S3 de minutos (#51). El caller escapa cancelando
    /// (dropeando el future) o desarmando el gate (`false` / [`Self::clear`],
    /// se observa en ≤20ms); las demás operaciones no se ven afectadas.
    pub fn hold_copy_native(&self, hold: bool) {
        self.lock().hold_copy_native = hold;
    }

    /// `true` si algún `copy_native` ya ENTRÓ en el gate: el test espera esta
    /// señal antes de cancelar — determinista, sin sleeps a ciegas.
    #[must_use]
    pub fn copy_native_entered(&self) -> bool {
        self.lock().copy_native_entered
    }

    pub(crate) async fn copy_native_gate(&self) {
        // Poll barato: compatible con `tokio::time::pause` y sin retener el
        // lock a través del await.
        self.lock().copy_native_entered = true;
        while self.lock().hold_copy_native {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
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

    /// Consume una carga de mutación ambigua, si está armada. Lo llama cada
    /// mutación de Mem JUSTO DESPUÉS de aplicar su efecto.
    pub(crate) fn take_ambiguous(&self) -> bool {
        let mut st = self.lock();
        if st.ambiguous_next > 0 {
            st.ambiguous_next -= 1;
            true
        } else {
            false
        }
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
