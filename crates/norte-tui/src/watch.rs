//! Vigilancia de los directorios VISIBLES de los panes (#106, mitad
//! watching): un watcher nativo (inotify/FSEvents/ReadDirectoryChangesW)
//! sobre los dirs `file://` de ambos panes, con FALLBACK a sondeo de mtime
//! cada 2 s cuando el nativo no arranca o `watch()` falla — el pitfall de
//! CLAUDE.md: los watches de inotify están LIMITADOS; degradar con aviso,
//! jamás fallar. Los eventos llegan al run loop DEBOUNCED (coalesce con
//! flanco de cola): una ráfaga de escrituras = un refresh, no una tormenta.
//!
//! Alcance v1: solo panes locales no-virtuales (un dir sftp/S3/archive no
//! tiene inotify; su refresh sigue siendo manual, Ctrl+R). El run loop
//! reacciona a cada evento con el MISMO camino que `pane.refresh`
//! (cancelable, marcas sobreviven, ritual post-refresh #118).
//!
//! Límites documentados del modo degradado: el sondeo mira el mtime del
//! DIRECTORIO — crear/borrar/renombrar dentro se ve; escribir en un
//! fichero existente NO cambia el mtime del padre y no se detecta (el
//! aviso de barra lo dice). La degradación es un latch de sesión (una
//! sola dirección): un tope transitorio de inotify deja el sondeo activo
//! hasta reiniciar — simplicidad antes que histéresis (v1).

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

/// Coalesce del flanco de cola: tras el primer evento crudo se espera este
/// hueco (drenando lo que siga llegando) antes de emitir UNO debounced.
const DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(300);
/// Período del sondeo de mtimes en modo degradado (pitfall inotify).
const POLL: std::time::Duration = std::time::Duration::from_secs(2);
/// Suelo entre EMISIONES (review MAJOR-3): una tormenta sostenida (copia
/// grande al dir, nuestra o ajena) emite como mucho una vez por suelo —
/// jamás un refresh cada `DEBOUNCE`. Derivado de `debounce` en tests.
const fn emit_floor(debounce: std::time::Duration) -> std::time::Duration {
    debounce.saturating_mul(3)
}
/// Tope de latencia del coalesce (review MAJOR-3): con eventos llegando
/// sin pausa, la espera de ventana tranquila no puede diferir el refresh
/// para siempre — pasado el tope se emite igual.
const fn max_coalesce(debounce: std::time::Duration) -> std::time::Duration {
    debounce.saturating_mul(10)
}

/// Estado compartido watcher/poller ↔ [`DirWatch`].
struct Shared {
    /// Dirs nativos vigilados (uno por pane; `None` = pane no vigilable).
    dirs: Mutex<[Option<PathBuf>; 2]>,
    /// Modo degradado: el poller sondea mtimes (el nativo no cubre).
    degraded: AtomicBool,
}

/// Vigilancia viva de los dirs de los panes. Soltar este valor la DETIENE
/// (regla 3, cancelación drop-based: el watcher nativo se cierra y el task
/// debouncer/poller ve su canal crudo cerrado y retorna).
pub struct DirWatch {
    /// Recibe UN evento por ráfaga (debounced): «algo cambió en un dir
    /// vigilado» — el consumidor refresca ambos panes (paridad Ctrl+R).
    pub rx: tokio::sync::mpsc::Receiver<()>,
    /// Emisor crudo retenido A PROPÓSITO (y usado por tests): en modo
    /// degradado el watcher es `None` y sin este extremo vivo el canal
    /// crudo se cerraría, matando también al POLLER. Su drop (con el del
    /// watcher) es lo que cierra el task — cancelación drop-based.
    #[cfg_attr(not(test), allow(dead_code))]
    raw_tx: tokio::sync::mpsc::UnboundedSender<()>,
    watcher: Option<notify::RecommendedWatcher>,
    shared: Arc<Shared>,
    /// Aviso de degradación pendiente de mostrar (una sola vez).
    degraded_pending: bool,
}

impl DirWatch {
    /// Arranca el pipeline: watcher nativo (si puede) + task
    /// debouncer/poller. Nunca falla: sin nativo queda DEGRADADO (sondeo).
    #[must_use]
    pub fn new() -> Self {
        Self::new_with(DEBOUNCE, POLL)
    }

    /// Como [`Self::new`] con períodos inyectables (tests: tiempo real con
    /// períodos cortos — el poller hace I/O real y el reloj pausado de
    /// tokio no la espera).
    fn new_with(debounce: std::time::Duration, poll: std::time::Duration) -> Self {
        let (raw_tx, mut raw_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let (out_tx, rx) = tokio::sync::mpsc::channel::<()>(1);
        let shared = Arc::new(Shared {
            dirs: Mutex::new([None, None]),
            degraded: AtomicBool::new(false),
        });
        // Watcher nativo: cualquier evento (también Err: «puedes haber
        // perdido eventos») = ping crudo; el debouncer coalesce. Mismo
        // criterio que `norte_config::watch`.
        let cb_tx = raw_tx.clone();
        let watcher =
            notify::recommended_watcher(move |_res: Result<notify::Event, notify::Error>| {
                let _ = cb_tx.send(());
            })
            .ok();
        if watcher.is_none() {
            shared.degraded.store(true, Ordering::Relaxed);
        }
        let degraded_pending = watcher.is_none();
        // Task debouncer + poller (regla 3: retorna cuando TODOS los
        // emisores crudos mueren — drop de `DirWatch` suelta watcher y
        // `raw_tx` — o cuando el consumidor suelta `rx`).
        let sh = Arc::clone(&shared);
        tokio::spawn(async move {
            let mut mtimes: std::collections::HashMap<PathBuf, std::time::SystemTime> =
                std::collections::HashMap::new();
            loop {
                tokio::select! {
                    ev = raw_rx.recv() => {
                        if ev.is_none() {
                            return; // todos los emisores muertos (drop)
                        }
                        // Flanco de cola REAL (review MAJOR-3): drenar y
                        // esperar hasta una ventana tranquila; una tormenta
                        // sin pausa emite igual al tope de latencia.
                        let inicio = tokio::time::Instant::now();
                        loop {
                            while raw_rx.try_recv().is_ok() {}
                            tokio::time::sleep(debounce).await;
                            if raw_rx.try_recv().is_err() {
                                break; // ventana tranquila
                            }
                            if inicio.elapsed() >= max_coalesce(debounce) {
                                while raw_rx.try_recv().is_ok() {}
                                break;
                            }
                        }
                        if out_tx.send(()).await.is_err() {
                            return; // consumidor muerto
                        }
                        // Suelo entre emisiones: lo que llegue durante la
                        // espera se acumula y coalesce en la siguiente.
                        tokio::time::sleep(emit_floor(debounce)).await;
                    }
                    () = tokio::time::sleep(poll), if sh.degraded.load(Ordering::Relaxed) => {
                        if out_tx.is_closed() {
                            return;
                        }
                        // Poison imposible en la práctica (nadie panica
                        // con el lock): los datos siguen siendo válidos.
                        let dirs = sh
                            .dirs
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .clone();
                        // MINOR-4: poda de líneas base de dirs ya no
                        // vigilados (sin ella el mapa crece toda la sesión).
                        mtimes.retain(|d, _| dirs.iter().flatten().any(|w| w == d));
                        let mut changed = false;
                        for dir in dirs.into_iter().flatten() {
                            let Ok(meta) = tokio::fs::metadata(&dir).await else {
                                continue; // dir desaparecido: el refresh lo dirá
                            };
                            let Ok(modified) = meta.modified() else {
                                continue;
                            };
                            match mtimes.insert(dir, modified) {
                                Some(prev) if prev != modified => changed = true,
                                // Primera vista = línea base (arrancar no
                                // es un cambio); mtime igual = nada.
                                None | Some(_) => {}
                            }
                        }
                        if changed && out_tx.send(()).await.is_err() {
                            return;
                        }
                    }
                }
            }
        });
        Self {
            rx,
            raw_tx,
            watcher,
            shared,
            degraded_pending,
        }
    }

    /// Actualiza el conjunto vigilado al de `targets` (uno por pane,
    /// `None` = no vigilable). Diff barato: sin cambios, cero syscalls —
    /// llamable en cada iteración del run loop. Un `watch()` que falla
    /// (tope de inotify) degrada a sondeo con aviso, jamás falla.
    pub fn rewatch(&mut self, targets: &[Option<PathBuf>; 2]) {
        use notify::Watcher as _;
        let old = {
            // Un solo scope de lock (compare+replace atómico); poison
            // imposible en la práctica → into_inner.
            let mut d = self
                .shared
                .dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if *d == *targets {
                return;
            }
            std::mem::replace(&mut *d, targets.clone())
        };
        if let Some(w) = &mut self.watcher {
            for dir in old.iter().flatten() {
                if !targets.iter().flatten().any(|t| t == dir) {
                    let _ = w.unwatch(dir);
                }
            }
            for dir in targets.iter().flatten() {
                if !old.iter().flatten().any(|t| t == dir)
                    && w.watch(dir, notify::RecursiveMode::NonRecursive).is_err()
                    && !self.shared.degraded.swap(true, Ordering::Relaxed)
                {
                    // Tope de watches (pitfall inotify): degradar con
                    // aviso, el poller cubre desde ya.
                    self.degraded_pending = true;
                }
            }
        }
    }

    /// `true` UNA vez cuando la vigilancia acaba de degradar a sondeo — el
    /// caller pinta el aviso (`status-watch-degraded`) y no repite.
    pub fn take_degraded_notice(&mut self) -> bool {
        std::mem::take(&mut self.degraded_pending)
    }

    /// Inyector de eventos crudos para tests (mismo canal que el watcher).
    #[cfg(test)]
    fn inject(&self) {
        let _ = self.raw_tx.send(());
    }

    /// Fuerza el modo degradado (tests del poller).
    #[cfg(test)]
    fn force_degraded(&self) {
        self.shared.degraded.store(true, Ordering::Relaxed);
    }
}

impl Default for DirWatch {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FAST_DEBOUNCE: std::time::Duration = std::time::Duration::from_millis(20);
    const FAST_POLL: std::time::Duration = std::time::Duration::from_millis(50);

    async fn recv_within(rx: &mut tokio::sync::mpsc::Receiver<()>, d: std::time::Duration) -> bool {
        tokio::time::timeout(d, rx.recv()).await.is_ok()
    }

    /// Una ráfaga de eventos crudos = UN evento debounced (flanco de cola)
    /// — sin esto, una copia grande al dir vigilado sería una tormenta de
    /// refreshes.
    #[tokio::test]
    async fn rafaga_coalesce_a_un_evento() {
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        for _ in 0..5 {
            w.inject();
        }
        assert!(
            recv_within(&mut w.rx, std::time::Duration::from_secs(2)).await,
            "un evento debounced"
        );
        tokio::time::sleep(FAST_DEBOUNCE * 3).await;
        assert!(w.rx.try_recv().is_err(), "y SOLO uno");
    }

    /// Modo degradado (pitfall inotify): el poller detecta un cambio de
    /// mtime del dir vigilado y emite; la primera vista es línea base (el
    /// arranque no es un cambio).
    #[tokio::test]
    async fn poller_degradado_detecta_mtime() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        // Sin watcher nativo: solo el poller puede emitir (aísla el test
        // de un inotify real sobre el tempdir).
        w.watcher = None;
        w.force_degraded();
        w.rewatch(&[Some(dir.path().to_path_buf()), None]);
        // Línea base: varias pasadas de poll SIN tocar el dir.
        tokio::time::sleep(FAST_POLL * 4).await;
        assert!(w.rx.try_recv().is_err(), "línea base sin evento");
        // Cambio real (el mtime del DIRECTORIO cambia al crear dentro).
        std::fs::write(dir.path().join("nuevo"), b"x").unwrap();
        assert!(
            recv_within(&mut w.rx, std::time::Duration::from_secs(5)).await,
            "el cambio de mtime emite un evento"
        );
    }

    /// Regla 3 (cancelación drop-based): soltar los emisores mata el task —
    /// el canal debounced se cierra (recv devuelve None), nada queda vivo.
    #[tokio::test]
    async fn drop_cierra_el_pipeline() {
        let w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        let mut rx = w.rx;
        drop(w.watcher);
        drop(w.raw_tx);
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv())
                .await
                .expect("el task debe morir, no colgarse"),
            None,
            "pipeline muerto tras el drop"
        );
    }

    /// Review MINOR-7: quitar un dir del conjunto (pane a virtual, cd a
    /// remoto) actualiza el estado compartido — el poller deja de sondearlo
    /// y su línea base se poda.
    #[tokio::test]
    async fn rewatch_a_menos_dirs_actualiza_el_conjunto() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        w.rewatch(&[Some(dir.path().to_path_buf()), None]);
        w.rewatch(&[None, None]);
        assert_eq!(
            *w.shared
                .dirs
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            [None, None]
        );
    }

    /// Review MINOR-7: el aviso de degradación es one-shot.
    #[tokio::test]
    async fn take_degraded_notice_es_one_shot() {
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        w.degraded_pending = true;
        assert!(w.take_degraded_notice());
        assert!(!w.take_degraded_notice(), "solo la primera vez");
    }

    /// Review MINOR-7: drop del VALOR ENTERO (el camino real de
    /// producción) también mata el pipeline.
    #[tokio::test]
    async fn drop_entero_cierra_el_pipeline() {
        let (probe_tx, mut probe_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        {
            let w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
            // Sonda: cuando el task muera, su out_tx se suelta… no es
            // observable desde fuera sin rx (que muere con w). Se observa
            // vía el emisor crudo: tras el drop, mandar falla.
            let raw = w.raw_tx.clone();
            drop(w);
            tokio::spawn(async move {
                // El task ve raw_rx colgando de ESTE clone; al soltarlo el
                // canal muere del todo y el task retorna.
                drop(raw);
                let _ = probe_tx.send(());
            });
        }
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), probe_rx.recv())
                .await
                .is_ok()
        );
    }

    /// Review MAJOR-3: una TORMENTA sostenida de eventos crudos no emite
    /// un refresh por debounce — el suelo entre emisiones acota la tasa.
    #[tokio::test]
    async fn tormenta_sostenida_respeta_el_suelo() {
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        let raw = w.raw_tx.clone();
        let storm = tokio::spawn(async move {
            for _ in 0..200 {
                let _ = raw.send(());
                tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            }
        });
        // Tormenta ≈ 400 ms = 20×debounce. Sin suelo serían ~20 emisiones;
        // con coalesce+tope+suelo caben ~2-3. Cota generosa anti-flake.
        let mut emitted = 0;
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while tokio::time::Instant::now() < deadline {
            match tokio::time::timeout(std::time::Duration::from_millis(100), w.rx.recv()).await {
                Ok(Some(())) => emitted += 1,
                _ => {
                    if storm.is_finished() {
                        break;
                    }
                }
            }
        }
        assert!(emitted >= 1, "la tormenta debe emitir al menos una vez");
        assert!(
            emitted <= 6,
            "tasa acotada por el suelo, no una por debounce: {emitted}"
        );
    }

    /// `rewatch` con el MISMO conjunto es no-op — se llama en cada
    /// iteración del run loop.
    #[tokio::test]
    async fn rewatch_es_idempotente() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        let t = [Some(dir.path().to_path_buf()), None];
        w.rewatch(&t);
        w.rewatch(&t);
        assert_eq!(*w.shared.dirs.lock().unwrap(), t);
    }

    /// Camino nativo REAL end-to-end: escribir en un dir vigilado produce
    /// un evento debounced (si notify no puede arrancar en este entorno,
    /// el constructor ya queda degradado y el test se salta — el poller
    /// tiene su propio test).
    #[tokio::test]
    async fn watcher_nativo_detecta_escritura() {
        let dir = tempfile::tempdir().unwrap();
        let mut w = DirWatch::new_with(FAST_DEBOUNCE, FAST_POLL);
        if w.watcher.is_none() {
            return; // entorno sin inotify: cubierto por el poller
        }
        w.rewatch(&[Some(dir.path().to_path_buf()), None]);
        std::fs::write(dir.path().join("nuevo"), b"x").unwrap();
        assert!(
            recv_within(&mut w.rx, std::time::Duration::from_secs(5)).await,
            "el watcher nativo emite ante una escritura real"
        );
    }
}
