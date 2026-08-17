//! Un plazo para una syscall que no se puede cancelar.
//!
//! Vive aquí, y no en cada crate que lo necesita, porque el motivo por el que
//! existe no es de nadie en particular: **un montaje de red muerto no se
//! cancela**. Un `statfs` sobre un NFS caído entra en D-state y no sale hasta
//! que el montaje conteste o alguien lo desmonte a la fuerza; no hay señal, no
//! hay token, y `spawn_blocking` no cancela nada — solo elige quién espera.

use std::time::Duration;

/// Corre `query` (bloqueante) en un [`std::thread`] DESACOPLADO, no en
/// [`tokio::task::spawn_blocking`], y se rinde a los `deadline`.
///
/// # Por qué un hilo suelto y no el pool
///
/// El pool de bloqueo de tokio está ACOTADO (512 hilos por defecto) y es
/// COMPARTIDO con todo lo demás del proceso, incluido cada `spawn_blocking`
/// que un provider hace para trabajo de disco corriente. Una consulta a un
/// montaje muerto se cuelga indefinidamente y no hay forma de cancelar una
/// syscall en vuelo: si eso pasara en el pool, un montaje caído ataría una
/// plaza mientras siguiera caído, y unos cuantos dejarían sin pool al daemon
/// para CUALQUIER otra operación local.
///
/// Un `std::thread` cuesta un hilo del sistema filtrado por sonda colgada en
/// vez de una plaza de un recurso compartido — peor en aislamiento (pila
/// nueva, nunca reutilizada) y mucho mejor en conjunto, porque no puede
/// bloquear a nadie más. Y el `oneshot` del que el `timeout` se marcha deja
/// que el runtime se apague sin esperarlo, cosa que una task de
/// `spawn_blocking` no permite: el runtime la espera al cerrar aunque su
/// llamante ya no.
///
/// `None` = no contestó a tiempo. Qué significa eso lo decide el llamante:
/// para los volúmenes es «este montaje no dice su tamaño», y para
/// `capabilities_at` es «me quedo con lo que el provider declara». Esta
/// función no tiene opinión.
///
/// ```
/// use norte_vfs::deadline::blocking_with_deadline;
/// use std::time::Duration;
///
/// let rt = tokio::runtime::Builder::new_current_thread()
///     .enable_time()
///     .build()
///     .expect("runtime");
/// rt.block_on(async {
///     // Contesta a tiempo.
///     assert_eq!(blocking_with_deadline(|| 7, Duration::from_secs(5)).await, Some(7));
///
///     // No contesta: el llamante sigue vivo, y el hilo se queda a solas con
///     // su syscall hasta que el sistema lo suelte.
///     let tarde = blocking_with_deadline(
///         || std::thread::sleep(Duration::from_secs(30)),
///         Duration::from_millis(20),
///     )
///     .await;
///     assert!(tarde.is_none());
/// });
/// ```
pub async fn blocking_with_deadline<T, F>(query: F, deadline: Duration) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce() -> T + Send + 'static,
{
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        // El receptor puede haberse ido ya (venció el plazo y el `timeout`
        // soltó su mitad): que `send` falle solo significa que ya no escucha
        // nadie, no que haya pasado nada malo.
        let _ = tx.send(query());
    });
    tokio::time::timeout(deadline, rx)
        .await
        .ok()
        .and_then(Result::ok)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lo corriente: contesta y se devuelve su respuesta.
    #[tokio::test]
    async fn una_respuesta_a_tiempo_llega_entera() {
        assert_eq!(
            blocking_with_deadline(|| "hola", Duration::from_secs(5)).await,
            Some("hola")
        );
    }

    /// Y lo que importa: quien pregunta NO se queda colgado con el hilo.
    ///
    /// La medida es del llamante, no del hilo: el hilo sigue dentro de su
    /// syscall —no hay forma de cancelarla— y de eso trata la función. Lo que
    /// se comprueba es que el `await` vuelve en el plazo y no en los 30
    /// segundos del bloqueo.
    #[tokio::test]
    async fn quien_pregunta_vuelve_en_el_plazo_aunque_el_hilo_siga() {
        let t0 = std::time::Instant::now();
        let r = blocking_with_deadline(
            || std::thread::sleep(Duration::from_secs(30)),
            Duration::from_millis(50),
        )
        .await;
        let dt = t0.elapsed();
        assert!(r.is_none(), "no contestó a tiempo: la respuesta es `None`");
        assert!(
            dt < Duration::from_secs(5),
            "el llamante volvió en {dt:?}, o sea que se quedó esperando al hilo"
        );
    }

    /// Un pánico dentro de la consulta es «no contestó», no un pánico del
    /// llamante: el hilo se muere con su `Sender` y el receptor lee un canal
    /// cerrado. Sin esto, una sonda de plataforma que reventara se llevaría por
    /// delante al que preguntó, que es la tarea del daemon.
    #[tokio::test]
    async fn un_panico_en_la_consulta_es_no_contesto() {
        let r = blocking_with_deadline(
            || -> u8 { panic!("la sonda revienta") },
            Duration::from_secs(5),
        )
        .await;
        assert_eq!(r, None);
    }
}
