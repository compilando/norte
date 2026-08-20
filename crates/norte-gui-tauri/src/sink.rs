//! El bombeo: del host a la ventana, EN ORDEN y sin inventarse nada.
//!
//! Está detrás de un trait a propósito. Lo que hay que probar aquí —que la
//! secuencia sale en orden, que un suscriptor que se queda atrás recibe el
//! aviso en vez de un parche imposible, y que al morir el host se deja de
//! emitir— no necesita ni pantalla ni `WebKitGTK`: necesita un sumidero que
//! apunte lo que le llega.

use norte_ui_host::{BridgeEnvelope, UiSubscription, Update, dto::UiUpdate};

/// El evento por el que viajan las actualizaciones.
pub const EVENT_UPDATE: &str = "norte://update";

/// El evento que dice «te has quedado atrás, pide una foto».
pub const EVENT_LAGGED: &str = "norte://lagged";

/// A dónde van las actualizaciones.
pub trait UpdateSink: Send + 'static {
    /// Manda una actualización. Un fallo PARA el bombeo: si la ventana ya no
    /// recibe, seguir serializando es trabajo para nadie.
    ///
    /// # Errors
    /// [`SinkError`] cuando la ventana ya no acepta eventos.
    fn update(&self, env: &BridgeEnvelope<UiUpdate>) -> Result<(), SinkError>;

    /// Avisa de que el suscriptor se quedó atrás.
    ///
    /// # Errors
    /// [`SinkError`] cuando la ventana ya no acepta eventos.
    fn lagged(&self) -> Result<(), SinkError>;
}

/// La ventana ya no recibe.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("el sumidero de actualizaciones no acepta más: {0}")]
pub struct SinkError(pub String);

/// Drena la suscripción hacia el sumidero hasta que uno de los dos se acabe.
///
/// No reordena, no fusiona y no descarta: el orden ES la garantía que el
/// bridge promete (ADR 0066, decisión D7). Lo único que traduce es el
/// `Lagged` del canal, que no es una actualización sino una instrucción.
pub async fn pump<S: UpdateSink>(mut sub: UiSubscription, sink: S) {
    while let Some(u) = sub.recv().await {
        let enviado = match u {
            Update::Message(env) => sink.update(&env),
            Update::Lagged => sink.lagged(),
        };
        if enviado.is_err() {
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
    struct Falso {
        vistos: Arc<Mutex<Vec<String>>>,
        muerto: bool,
    }

    impl UpdateSink for Falso {
        fn update(&self, env: &BridgeEnvelope<UiUpdate>) -> Result<(), SinkError> {
            if self.muerto {
                return Err(SinkError("ventana cerrada".to_owned()));
            }
            self.vistos
                .lock()
                .expect("lock sano")
                .push(format!("u{}", env.sequence));
            Ok(())
        }

        fn lagged(&self) -> Result<(), SinkError> {
            self.vistos
                .lock()
                .expect("lock sano")
                .push("lag".to_owned());
            Ok(())
        }
    }

    fn aviso(seq: u64) -> BridgeEnvelope<UiUpdate> {
        BridgeEnvelope::new(
            InstanceId::new("i"),
            seq,
            UiUpdate::Notice(UiNotice::Message {
                key: "k".to_owned(),
                detail: None,
            }),
        )
    }

    /// Lo que entra por la suscripción sale en el MISMO orden.
    #[tokio::test]
    async fn el_orden_se_respeta() {
        let (host, _snap) = crate::commands::tests_soporte::host_de_prueba().await;
        let sub = host.subscribe();
        let vistos = Arc::new(Mutex::new(Vec::new()));
        let sink = Falso {
            vistos: Arc::clone(&vistos),
            muerto: false,
        };
        let bombeo = tokio::spawn(pump(sub, sink));
        for _ in 0..3 {
            host.dispatch(norte_ui_host::UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await
            .expect("host vivo");
        }
        host.shutdown().await.expect("apaga");
        // El asa del host CONSERVA el emisor de actualizaciones: mientras
        // viva, el bombeo espera. Soltarla es lo que cierra el canal — y es
        // lo que hace el proceso de verdad al cerrar la ventana.
        drop(host);
        tokio::time::timeout(std::time::Duration::from_secs(5), bombeo)
            .await
            .expect("el bombeo termina cuando el host se va")
            .expect("sin panic");
        let v = vistos.lock().expect("lock sano").clone();
        let secuencias: Vec<&String> = v.iter().filter(|s| s.starts_with('u')).collect();
        assert!(secuencias.len() >= 3, "llegaron las tres: {v:?}");
        let mut ordenadas = secuencias.clone();
        ordenadas.sort_by_key(|s| s[1..].parse::<u64>().unwrap_or(0));
        assert_eq!(secuencias, ordenadas, "y en orden: {v:?}");
    }

    /// Una ventana que ya no recibe PARA el bombeo; no se serializa contra
    /// una pared.
    #[tokio::test]
    async fn una_ventana_muerta_para_el_bombeo() {
        let (tx, rx) = tokio::sync::broadcast::channel(8);
        drop(rx);
        let _ = tx.send(aviso(1));
        let (host, _snap) = crate::commands::tests_soporte::host_de_prueba().await;
        let sub = host.subscribe();
        let sink = Falso {
            vistos: Arc::new(Mutex::new(Vec::new())),
            muerto: true,
        };
        let bombeo = tokio::spawn(pump(sub, sink));
        host.dispatch(norte_ui_host::UiAction::MoveCursor {
            slot_id: 1,
            delta: 1,
        })
        .await
        .expect("host vivo");
        let fin = tokio::time::timeout(std::time::Duration::from_secs(5), bombeo).await;
        assert!(fin.is_ok(), "el bombeo termina en cuanto la ventana falla");
        drop(host);
    }
}
