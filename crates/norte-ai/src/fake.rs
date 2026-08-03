//! Proveedor de embeddings DETERMINISTA para tests (feature `testutil`).
//!
//! El vector deriva de un hash del texto: mismo texto ⇒ mismo vector, textos
//! distintos ⇒ vectores no correlacionados. Registra cada batch recibido
//! (`calls`) para que los tests afirmen QUÉ salió hacia el proveedor (p. ej.
//! que un path bajo `denied_prefixes` jamás aparece).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use crate::{AiCaps, AiError, AiProvider, ChatRequest, ChatStream, ModelInfo};

/// Proveedor fake: `embed` determinista, `chat` no soportado.
pub struct FakeEmbed {
    dim: usize,
    /// Lo que reporta `is_local()` (default `true`).
    pub local: bool,
    /// Batches recibidos, en orden. Se registra ANTES de aplicar fallos
    /// inyectados: una llamada rate-limited también cuenta como intento.
    pub calls: Mutex<Vec<Vec<String>>>,
    /// Latencia artificial por llamada (ventana para tests de cancelación).
    pub delay: Option<Duration>,
    /// `retry_after` que reportan los [`AiError::RateLimited`] inyectados.
    /// Default `Some(0)`: reintento inmediato (tests rápidos); un valor alto
    /// abre ventana para tests de cancelación durante la espera de reintento.
    pub retry_after: Option<u64>,
    rate_limited_budget: AtomicU32,
}

impl FakeEmbed {
    /// Fake de dimensión `dim`, local, sin fallos.
    #[must_use]
    pub fn new(dim: usize) -> Self {
        Self {
            dim,
            local: true,
            calls: Mutex::new(Vec::new()),
            delay: None,
            retry_after: Some(0),
            rate_limited_budget: AtomicU32::new(0),
        }
    }

    /// Las próximas `n` llamadas a `embed` devuelven [`AiError::RateLimited`].
    #[must_use]
    pub fn with_rate_limited(self, n: u32) -> Self {
        self.rate_limited_budget.store(n, Ordering::SeqCst);
        self
    }

    /// Latencia artificial antes de responder.
    #[must_use]
    pub fn with_delay(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }

    fn vec_for(&self, text: &str) -> Vec<f32> {
        // Semilla FNV-1a + xorshift64: determinista y sin dependencias. Cada
        // componente sale de 16 bits del estado vía conversiones SIN pérdida
        // (u16→f32 es `From`), evitando casts que disparen pedantic.
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in text.as_bytes() {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        let mut state = h | 1;
        let mut v: Vec<f32> = (0..self.dim)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                let bytes = state.to_le_bytes();
                let bits = u16::from_le_bytes([bytes[6], bytes[7]]);
                // [0, 1] → [-1, 1]
                (f32::from(bits) / f32::from(u16::MAX)).mul_add(2.0, -1.0)
            })
            .collect();
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

#[async_trait]
impl AiProvider for FakeEmbed {
    fn id(&self) -> &'static str {
        "fake-embed"
    }

    fn capabilities(&self) -> AiCaps {
        AiCaps::EMBEDDINGS
    }

    fn is_local(&self) -> bool {
        self.local
    }

    async fn chat(&self, _req: ChatRequest) -> Result<ChatStream, AiError> {
        Err(AiError::Unsupported)
    }

    async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>, AiError> {
        // Se registra ANTES de la latencia: "qué salió hacia el proveedor" se
        // decide al llamar, y un future cancelado en mitad del delay también
        // debe constar como intento.
        // Invariante: el lock solo se envenena si un test hizo panic con él
        // tomado; propagar ese panic es lo correcto en un fake de test.
        self.calls.lock().expect("test lock").push(inputs.to_vec());
        if let Some(d) = self.delay {
            tokio::time::sleep(d).await;
        }
        if self
            .rate_limited_budget
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            return Err(AiError::RateLimited {
                retry_after: self.retry_after,
            });
        }
        Ok(inputs.iter().map(|t| self.vec_for(t)).collect())
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, AiError> {
        Ok(Vec::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::AiProvider;

    #[tokio::test]
    async fn deterministic_and_records_inputs() {
        let f = FakeEmbed::new(8);
        let a = f.embed(&["hola".into(), "mundo".into()]).await.unwrap();
        let b = f.embed(&["hola".into()]).await.unwrap();
        assert_eq!(a[0], b[0]); // mismo texto ⇒ mismo vector
        assert_ne!(a[0], a[1]); // texto distinto ⇒ vector distinto
        assert_eq!(a[0].len(), 8);
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], vec!["hola".to_string(), "mundo".to_string()]);
    }

    #[tokio::test]
    async fn rate_limit_budget_then_ok() {
        let f = FakeEmbed::new(4).with_rate_limited(1);
        assert!(matches!(
            f.embed(&["x".into()]).await,
            Err(crate::AiError::RateLimited { .. })
        ));
        assert!(f.embed(&["x".into()]).await.is_ok());
        assert_eq!(f.calls.lock().unwrap().len(), 2);
    }
}
