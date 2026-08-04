//! Live config reload plumbing (feature `watch`): a native filesystem
//! watcher (inotify/FSEvents/ReadDirectoryChangesW) with a polling
//! fallback, so frontends can hot-reload `norte.toml`/`keymap.toml`/
//! `openers.toml` without blocking their async runtime.

use std::path::{Path, PathBuf};

use crate::dirs::Layers;

/// Modo de vigilancia logrado (para el aviso al usuario, ADR 0007).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchMode {
    /// Watcher nativo (inotify/FSEvents/ReadDirectoryChangesW).
    Notify,
    /// Degradado a sondeo de mtimes cada 2 s (con aviso, jamás fallar).
    Polling,
}

/// Vigilancia viva de los dirs de config: soltar este valor la DETIENE en
/// ambos modos (el watcher nativo se cierra; el task de polling se cancela
/// vía token — regla 3).
pub struct Watch {
    /// Cómo se está vigilando.
    pub mode: WatchMode,
    _watcher: Option<notify::RecommendedWatcher>,
    cancel: tokio_util::sync::CancellationToken,
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Vigila los directorios de capas y envía `()` por `tx` en cada cambio
/// (sin debounce: eso es del consumidor). Watcher nativo si arranca — y
/// SIEMPRE un poll lento de respaldo (capas cuyo dir aún no existe, colas
/// de inotify desbordadas); si el nativo no arranca en absoluto, el poll
/// pasa a rápido y `mode` lo delata para el aviso (trampa documentada:
/// degradar, jamás fallar). El setup (stats + inotify) corre en
/// `spawn_blocking` — llamable desde async (regla 2).
pub async fn watch(layers: &Layers, tx: tokio::sync::mpsc::Sender<()>) -> Watch {
    use notify::Watcher;
    let layers2 = layers.clone();
    let tx2 = tx.clone();
    let watcher = tokio::task::spawn_blocking(move || {
        let mut watcher = notify::recommended_watcher({
            move |res: Result<notify::Event, notify::Error>| {
                // El watcher nativo vigila el DIRECTORIO entero (no hay
                // vigilancia por fichero portable), y el dir de config del
                // usuario tiene vecinos VIVOS: `index.db` (índice
                // semántico), `journal.db`/`-shm`, el lockfile de los
                // persist. `SQLite` escribe ahí mientras la app corre, así
                // que sin este filtro cada escritura disparaba un
                // hot-reload completo — que cierra la ayuda (F1) y la
                // paleta abiertas y pinta «config recargada». Solo cuentan
                // las tres capas TOML, las MISMAS que mira el snapshot del
                // polling ([`CONFIG_FILES`]).
                //
                // También en Err (M3 de la revisión): un error de notify
                // significa "puedes haber perdido eventos" — releer TODO es
                // exactamente la respuesta correcta, y un evento SIN rutas
                // (backends que no las traen) se trata igual de
                // conservador. try_send: los cambios se coalescen; perder
                // uno con el canal lleno es inocuo.
                let interesa = match &res {
                    Err(_) => true,
                    // `Access(_)` es LECTURA (incluido el open que hace el
                    // propio reload al releer las capas): no cambia nada y,
                    // sin descartarlo, el reload se realimentaba —
                    // recargar abre norte.toml, el open dispara otro
                    // reload. Toda escritura llega igualmente como
                    // `Create`/`Modify`/`Remove`.
                    Ok(ev) => {
                        !matches!(ev.kind, notify::EventKind::Access(_))
                            && (ev.paths.is_empty() || ev.paths.iter().any(|p| is_config_file(p)))
                    }
                };
                if interesa {
                    let _ = tx2.try_send(());
                }
            }
        })
        .ok()?;
        let mut watching = false;
        for (dir, _kind) in &layers2.dirs {
            if dir.is_dir()
                && watcher
                    .watch(dir, notify::RecursiveMode::NonRecursive)
                    .is_ok()
            {
                watching = true;
            }
        }
        watching.then_some(watcher)
    })
    .await
    .ok()
    .flatten();

    let mode = if watcher.is_some() {
        WatchMode::Notify
    } else {
        WatchMode::Polling
    };
    // Poll de respaldo: rápido si es el ÚNICO mecanismo; lento como red de
    // seguridad del nativo (dirs creados en caliente, eventos perdidos).
    let period = match mode {
        WatchMode::Polling => std::time::Duration::from_secs(2),
        WatchMode::Notify => std::time::Duration::from_secs(10),
    };
    let cancel = tokio_util::sync::CancellationToken::new();
    spawn_poll(layers.clone(), tx, period, cancel.clone());
    Watch {
        mode,
        _watcher: watcher,
        cancel,
    }
}

/// Vigilancia por POLLING puro con periodo propio (el mecanismo del
/// fallback de [`watch`], expuesto para poder testear su cancelación).
///
/// # Panics
/// Si se llama fuera de un runtime tokio (hace `tokio::spawn`).
#[doc(hidden)]
#[must_use]
pub fn watch_polling(
    layers: &Layers,
    tx: tokio::sync::mpsc::Sender<()>,
    period: std::time::Duration,
) -> Watch {
    let cancel = tokio_util::sync::CancellationToken::new();
    spawn_poll(layers.clone(), tx, period, cancel.clone());
    Watch {
        mode: WatchMode::Polling,
        _watcher: None,
        cancel,
    }
}

/// Task de polling de mtimes+tamaños, cancelable (regla 3).
fn spawn_poll(
    layers: Layers,
    tx: tokio::sync::mpsc::Sender<()>,
    period: std::time::Duration,
    cancel: tokio_util::sync::CancellationToken,
) {
    tokio::spawn(async move {
        let mut last: Option<Vec<(PathBuf, std::time::SystemTime, u64)>> = None;
        loop {
            tokio::select! {
                () = cancel.cancelled() => return,
                () = tokio::time::sleep(period) => {}
            }
            let layers2 = layers.clone();
            let Ok(snapshot) = tokio::task::spawn_blocking(move || snapshot(&layers2)).await else {
                return;
            };
            if let Some(prev) = &last
                && *prev != snapshot
                && tx.send(()).await.is_err()
            {
                return;
            }
            last = Some(snapshot);
        }
    });
}

/// Las capas TOML que una edición puede cambiar: lo ÚNICO que dispara un
/// hot-reload, tanto por el watcher nativo (que solo puede vigilar el dir
/// entero) como por el snapshot del polling. Cualquier otro fichero del dir
/// de config —`index.db`, `journal.db`, lockfiles— es ruido de la propia
/// app.
const CONFIG_FILES: [&str; 3] = ["norte.toml", "keymap.toml", "openers.toml"];

/// ¿Es `p` una de las capas de [`CONFIG_FILES`]? Por NOMBRE de fichero: el
/// watcher entrega rutas absolutas del dir vigilado, y un rename de
/// `norte.toml.tmp` a `norte.toml` (cómo escriben los editores, y cómo
/// escribe el propio persist atómico) llega con el destino entre sus rutas.
fn is_config_file(p: &Path) -> bool {
    p.file_name()
        .is_some_and(|n| CONFIG_FILES.iter().any(|c| n == *c))
}

/// Snapshot de (mtime, tamaño) de los archivos de config presentes — el
/// tamaño caza escrituras dentro de la granularidad del mtime del FS.
fn snapshot(layers: &Layers) -> Vec<(PathBuf, std::time::SystemTime, u64)> {
    let mut out = Vec::new();
    for (dir, _kind) in &layers.dirs {
        // "openers.toml" se añade aquí (deuda pre-existente cerrada al
        // copiar este módulo a norte-config): el watcher nativo vigila el
        // dir ENTERO y ya captaba sus ediciones, pero el fallback de
        // polling solo miraba norte.toml/keymap.toml — un openers.toml
        // editado bajo polling puro (watcher nativo caído) se perdía.
        for name in CONFIG_FILES {
            let p = dir.join(name);
            if let Ok(md) = std::fs::metadata(&p)
                && let Ok(m) = md.modified()
            {
                let len = md.len();
                out.push((p, m, len));
            }
        }
    }
    out
}
