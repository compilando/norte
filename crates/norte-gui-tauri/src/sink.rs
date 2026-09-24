//! The pump: from the host to the window, IN ORDER and without making
//! anything up.
//!
//! It sits behind a trait on purpose. What has to be tested here — that the
//! sequence comes out in order, that a subscriber that falls behind gets the
//! notice instead of an impossible patch, and that emitting stops once the
//! host dies — needs neither a screen nor `WebKitGTK`: it needs a sink that
//! records what reaches it.

use norte_ui_host::{BridgeEnvelope, UiSubscription, Update, dto::UiUpdate};

/// The event updates travel through.
pub const EVENT_UPDATE: &str = "norte://update";

/// The event that says "you fell behind, ask for a frame".
pub const EVENT_LAGGED: &str = "norte://lagged";

/// The event that says "the catalogue changed, ask for it again".
///
/// Today only the THEME triggers it. It travels as a notice and not with the
/// catalogue inside because the catalogue already has its own command, and
/// sending it two ways would be two ways of having a different version of the
/// same thing.
pub const EVENT_CATALOG: &str = "norte://catalog";

/// Where the updates go.
pub trait UpdateSink: Send + 'static {
    /// Sends an update. A failure STOPS the pump: if the window no longer
    /// receives, continuing to serialize is work for nobody.
    ///
    /// # Errors
    /// [`SinkError`] when the window no longer accepts events.
    fn update(&self, env: &BridgeEnvelope<UiUpdate>) -> Result<(), SinkError>;

    /// Notifies that the subscriber fell behind.
    ///
    /// # Errors
    /// [`SinkError`] when the window no longer accepts events.
    fn lagged(&self) -> Result<(), SinkError>;
}

/// The window no longer receives.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the update sink no longer accepts anything: {0}")]
pub struct SinkError(pub String);

/// Drains the subscription into the sink until one of the two ends.
///
/// It does not reorder, does not merge and does not drop: order IS the
/// guarantee the bridge promises (ADR 0066, decision D7). The only thing it
/// translates is the channel's `Lagged`, which is not an update but an
/// instruction.
pub async fn pump<S: UpdateSink>(mut sub: UiSubscription, sink: S) {
    while let Some(u) = sub.recv().await {
        let sent = match u {
            Update::Message(env) => sink.update(&env),
            Update::Lagged => sink.lagged(),
        };
        if sent.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use norte_ui_host::InstanceId;
    use norte_ui_host::dto::UiNotice;

    use super::*;

    #[derive(Default)]
    struct Fake {
        seen: Arc<Mutex<Vec<String>>>,
        dead: bool,
    }

    impl UpdateSink for Fake {
        fn update(&self, env: &BridgeEnvelope<UiUpdate>) -> Result<(), SinkError> {
            if self.dead {
                return Err(SinkError("window closed".to_owned()));
            }
            self.seen
                .lock()
                .expect("sound lock")
                .push(format!("u{}", env.sequence));
            Ok(())
        }

        fn lagged(&self) -> Result<(), SinkError> {
            self.seen.lock().expect("sound lock").push("lag".to_owned());
            Ok(())
        }
    }

    fn notice(seq: u64) -> BridgeEnvelope<UiUpdate> {
        BridgeEnvelope::new(
            InstanceId::new("i"),
            seq,
            UiUpdate::Notice(UiNotice::Message {
                key: "k".to_owned(),
                detail: None,
            }),
        )
    }

    /// What comes in through the subscription comes out in the SAME order.
    #[tokio::test]
    async fn order_is_respected() {
        let (host, _snap) = crate::commands::tests_soporte::host_de_prueba().await;
        let sub = host.subscribe();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = Fake {
            seen: Arc::clone(&seen),
            dead: false,
        };
        let pump_task = tokio::spawn(pump(sub, sink));
        for _ in 0..3 {
            host.dispatch(norte_ui_host::UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await
            .expect("host is alive");
        }
        host.shutdown().await.expect("shuts down");
        // The host's handle KEEPS the update emitter alive: while it lives,
        // the pump waits. Dropping it is what closes the channel — and that
        // is what the real process does when it closes the window.
        drop(host);
        tokio::time::timeout(std::time::Duration::from_secs(5), pump_task)
            .await
            .expect("the pump ends when the host goes away")
            .expect("no panic");
        let v = seen.lock().expect("sound lock").clone();
        let sequences: Vec<&String> = v.iter().filter(|s| s.starts_with('u')).collect();
        assert!(sequences.len() >= 3, "all three arrived: {v:?}");
        let mut sorted = sequences.clone();
        sorted.sort_by_key(|s| s[1..].parse::<u64>().unwrap_or(0));
        assert_eq!(sequences, sorted, "and in order: {v:?}");
    }

    /// A window that no longer receives STOPS the pump; it does not keep
    /// serializing against a wall.
    #[tokio::test]
    async fn a_dead_window_stops_the_pump() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        drop(rx);
        let _ = tx.send(notice(1));
        let (host, _snap) = crate::commands::tests_soporte::host_de_prueba().await;
        let sub = host.subscribe();
        let sink = Fake {
            seen: Arc::new(Mutex::new(Vec::new())),
            dead: true,
        };
        let pump_task = tokio::spawn(pump(sub, sink));
        host.dispatch(norte_ui_host::UiAction::MoveCursor {
            slot_id: 1,
            delta: 1,
        })
        .await
        .expect("host is alive");
        let end = tokio::time::timeout(std::time::Duration::from_secs(5), pump_task).await;
        assert!(end.is_ok(), "the pump ends as soon as the window fails");
        drop(host);
    }
}
