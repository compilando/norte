//! DETERMINISTIC embeddings provider for tests (feature `testutil`).
//!
//! The vector derives from a hash of the text: same text ⇒ same vector,
//! different texts ⇒ uncorrelated vectors. Records every batch received
//! (`calls`) so tests can assert WHAT went out to the provider (e.g. that a
//! path under `denied_prefixes` never appears).

use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use crate::{AiCaps, AiError, AiProvider, ChatRequest, ChatStream, ModelInfo};

/// Fake provider: deterministic `embed`, `chat` unsupported.
pub struct FakeEmbed {
    dim: usize,
    /// What `is_local()` reports (default `true`).
    pub local: bool,
    /// Batches received, in order. Recorded BEFORE applying injected
    /// failures: a rate-limited call also counts as an attempt.
    pub calls: Mutex<Vec<Vec<String>>>,
    /// Artificial latency per call (window for cancellation tests).
    pub delay: Option<Duration>,
    /// `retry_after` reported by the injected [`AiError::RateLimited`]s.
    /// Default `Some(0)`: immediate retry (fast tests); a high value opens a
    /// window for cancellation tests during the retry wait.
    pub retry_after: Option<u64>,
    rate_limited_budget: AtomicU32,
}

impl FakeEmbed {
    /// A fake of dimension `dim`, local, with no failures.
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

    /// The next `n` calls to `embed` return [`AiError::RateLimited`].
    #[must_use]
    pub fn with_rate_limited(self, n: u32) -> Self {
        self.rate_limited_budget.store(n, Ordering::SeqCst);
        self
    }

    /// Artificial latency before responding.
    #[must_use]
    pub fn with_delay(mut self, d: Duration) -> Self {
        self.delay = Some(d);
        self
    }

    fn vec_for(&self, text: &str) -> Vec<f32> {
        // FNV-1a seed + xorshift64: deterministic and dependency-free. Each
        // component comes from 16 bits of the state via LOSSLESS conversions
        // (u16→f32 is `From`), avoiding casts that would trigger pedantic.
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
        // Recorded BEFORE the latency: "what went out to the provider" is
        // decided at call time, and a future cancelled mid-delay must also
        // register as an attempt.
        // Invariant: the lock only gets poisoned if a test panicked while
        // holding it; propagating that panic is the right thing in a test
        // fake.
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
        let a = f.embed(&["hello".into(), "world".into()]).await.unwrap();
        let b = f.embed(&["hello".into()]).await.unwrap();
        assert_eq!(a[0], b[0]); // same text ⇒ same vector
        assert_ne!(a[0], a[1]); // different text ⇒ different vector
        assert_eq!(a[0].len(), 8);
        let calls = f.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0], vec!["hello".to_string(), "world".to_string()]);
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
