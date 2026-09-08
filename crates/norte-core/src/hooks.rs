//! Operation hooks (H1, ADR 0100): un plugin `hook` OBSERVA las entradas
//! que el journal ya registró, y lo único que puede devolver es una frase
//! para el humano.
//!
//! La fuente es el journal y no los handlers: [`crate::journal::Journal::
//! record_entry`] le ofrece cada fila comprometida a un [`HookSender`], así
//! que toda mutación —de cualquier frontend, de la CLI, de un agente, de un
//! lote, de un undo— llega por el mismo sitio (ADR 0077). El despachador
//! ([`spawn_dispatcher`]) vive FUERA del camino crítico: una cola acotada,
//! un drenado por tanda, un descubrimiento del registro por tanda (así una
//! aprobación recién dada vale en la siguiente) y una instancia por plugin y
//! tanda. Tres fallos seguidos apagan los hooks de ese plugin hasta que se
//! reactive, y se dice por el mismo canal que las frases.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use norte_plugin_host::{HOOK_EVENTS, PluginRuntime, hook_iface};
use norte_proto::methods::PluginNotice;
use tokio::sync::mpsc;

use crate::plugins::{LocationMint, LocationSession, PluginRegistry, guest_reason};

/// Cuántos eventos caben en la cola entre el journal y el despachador. Por
/// encima, [`HookSender::offer`] descarta el MÁS NUEVO y lo cuenta: la
/// mutación ya ocurrió y no se va a frenar por un observador lento.
pub const HOOK_QUEUE: usize = 1024;

/// Cuántos eventos se le entregan a un guest en una llamada como mucho. Es
/// también lo que acota el coste de un drenado: un lote de diez mil
/// renombrados llega en cuarenta llamadas, no en una ni en diez mil.
pub const HOOK_DRAIN_MAX: usize = 256;

/// Fallos SEGUIDOS —no instanció, atrapó, se pasó de presupuesto, rehusó—
/// tras los cuales los hooks de un plugin se apagan para el resto del
/// proceso. Un éxito entre medias pone el contador a cero.
pub const HOOK_FUSE_FAILURES: u32 = 3;

/// Una entrada del journal tal y como sale de `record_entry`: bytes de
/// cable, sin interpretar. Lo que el guest recibe se construye en el
/// despachador ([`to_wire_events`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookEvent {
    /// El `seq` asignado.
    pub seq: i64,
    /// Milisegundos UTC del registro.
    pub ts_ms: i64,
    /// `"created" | "removed" | "trashed" | "renamed" | "mode_changed"`.
    pub op: String,
    /// `"user" | "agent" | "plugin"`. El id del actor NO viaja (ADR 0100).
    pub actor_kind: String,
    /// Path afectado (bytes `to_wire`).
    pub path: Vec<u8>,
    /// Destino de un `renamed` / modo nuevo de un `mode_changed`.
    pub path_to: Option<Vec<u8>>,
    /// Lote, si formó parte de uno.
    pub batch_id: Option<i64>,
}

/// El extremo del journal: ofrece eventos sin esperar nunca.
#[derive(Debug, Clone)]
pub struct HookSender {
    tx: mpsc::Sender<HookEvent>,
    dropped: Arc<AtomicU64>,
}

impl HookSender {
    /// Encola `ev` si cabe. Nunca bloquea ni falla: con la cola llena el
    /// evento se descarta y se cuenta ([`Self::dropped`]); con el despachador
    /// muerto, se descarta en silencio — no queda nadie a quien avisar.
    pub fn offer(&self, ev: HookEvent) {
        if let Err(mpsc::error::TrySendError::Full(_)) = self.tx.try_send(ev) {
            let n = self.dropped.fetch_add(1, Ordering::Relaxed) + 1;
            // Una traza por potencia de dos: la primera dice que pasa, las
            // siguientes cuánto, y ninguna convierte una cola llena en un
            // registro lleno.
            if n.is_power_of_two() {
                tracing::warn!(dropped = n, "hooks: cola llena, eventos descartados");
            }
        }
    }

    /// Un par (extremo, receptor) sin despachador, para mirar lo que el
    /// journal ofrece.
    #[cfg(test)]
    pub(crate) fn for_test(capacity: usize) -> (Self, mpsc::Receiver<HookEvent>) {
        let (tx, rx) = mpsc::channel(capacity);
        (
            Self {
                tx,
                dropped: Arc::new(AtomicU64::new(0)),
            },
            rx,
        )
    }

    /// Cuántos eventos se descartaron por cola llena desde el arranque.
    #[must_use]
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
}

/// A dónde van los avisos: el daemon los difunde a humanos por
/// `plugin.notice`; un frontend embebido los empuja a su canal.
pub trait HookNoticeSink: Send + Sync {
    /// Un aviso, ya enmascarado y acotado.
    fn notice(&self, n: PluginNotice);
}

/// El fusible por plugin: cuenta fallos SEGUIDOS y apaga al llegar a
/// [`HOOK_FUSE_FAILURES`]. Puro, sin reloj, para poder probarlo.
#[derive(Debug, Default)]
pub struct Fuse {
    failures: HashMap<String, u32>,
    disabled: HashSet<String>,
}

impl Fuse {
    /// ¿Están apagados los hooks de `id`?
    #[must_use]
    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }

    /// Una llamada que fue bien: el contador vuelve a cero.
    pub fn record_ok(&mut self, id: &str) {
        self.failures.remove(id);
    }

    /// Una llamada que falló. Devuelve `true` la vez que APAGA los hooks del
    /// plugin (y solo esa vez), para avisar una vez y no en cada tanda.
    pub fn record_failure(&mut self, id: &str) -> bool {
        if self.disabled.contains(id) {
            return false;
        }
        let n = self.failures.entry(id.to_owned()).or_insert(0);
        *n += 1;
        if *n >= HOOK_FUSE_FAILURES {
            self.disabled.insert(id.to_owned());
            self.failures.remove(id);
            return true;
        }
        false
    }
}

/// Arranca el despachador y devuelve el extremo que se le instala al journal
/// ([`crate::Engine::enable_hooks`]). `config_dir` es donde viven
/// `plugins/` y `plugins-state.toml`; el registro se redescubre en cada tanda
/// para que una aprobación recién dada cuente sin reiniciar nada.
///
/// El despachador muere cuando muere el último [`HookSender`].
#[must_use]
pub fn spawn_dispatcher(
    config_dir: PathBuf,
    runtime: Arc<PluginRuntime>,
    sink: Arc<dyn HookNoticeSink>,
) -> HookSender {
    let (tx, mut rx) = mpsc::channel::<HookEvent>(HOOK_QUEUE);
    tokio::spawn(async move {
        let mut fuse = Fuse::default();
        while let Some(first) = rx.recv().await {
            let mut batch = vec![first];
            while batch.len() < HOOK_DRAIN_MAX {
                match rx.try_recv() {
                    Ok(ev) => batch.push(ev),
                    Err(_) => break,
                }
            }
            // Todo lo que sigue es I/O y CPU síncronos —descubrir el
            // registro, abrir directorios, correr wasm— así que va en
            // `spawn_blocking` (regla 2), y el fusible viaja con la tanda.
            let dir = config_dir.clone();
            let rt = Arc::clone(&runtime);
            let out = tokio::task::spawn_blocking(move || {
                let notices = dispatch_batch(&dir, &rt, &mut fuse, &batch);
                (fuse, notices)
            })
            .await;
            match out {
                Ok((f, notices)) => {
                    fuse = f;
                    for n in notices {
                        sink.notice(n);
                    }
                }
                Err(e) => {
                    tracing::error!(error = %e, "hooks: el despachador de una tanda murió");
                    fuse = Fuse::default();
                }
            }
        }
    });
    HookSender {
        tx,
        dropped: Arc::new(AtomicU64::new(0)),
    }
}

/// El nombre del evento del manifiesto para una op del journal, o `None`
/// para una op que este binario no sabe nombrar (un journal más nuevo).
#[must_use]
pub fn event_name_for(op: &str) -> Option<&'static str> {
    let wanted = format!("after-{}", op.replace('_', "-"));
    HOOK_EVENTS.iter().copied().find(|e| *e == wanted)
}

fn wire_op(op: &str) -> Option<hook_iface::Op> {
    Some(match op {
        "created" => hook_iface::Op::Created,
        "removed" => hook_iface::Op::Removed,
        "trashed" => hook_iface::Op::Trashed,
        "renamed" => hook_iface::Op::Renamed,
        "mode_changed" => hook_iface::Op::ModeChanged,
        _ => return None,
    })
}

fn wire_actor(kind: &str) -> Option<hook_iface::ActorKind> {
    Some(match kind {
        "user" => hook_iface::ActorKind::User,
        "agent" => hook_iface::ActorKind::Agent,
        "plugin" => hook_iface::ActorKind::Plugin,
        _ => return None,
    })
}

/// Los eventos de una tanda que `ons` pide, en la forma del guest. Con
/// `mint`, una sesión de ubicación por directorio PADRE distinto, que vive lo
/// que dure la llamada (las devuelve para que el llamante las sostenga).
fn to_wire_events(
    batch: &[HookEvent],
    ons: &[String],
    mint: Option<(&Arc<LocationMint>, Option<&str>)>,
) -> (Vec<hook_iface::Event>, Vec<LocationSession>) {
    let mut sessions: Vec<LocationSession> = Vec::new();
    let mut by_parent: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::new();
    for ev in batch {
        let Some(name) = event_name_for(&ev.op) else {
            continue;
        };
        if !ons.iter().any(|o| o == name) {
            continue;
        }
        let (Some(op), Some(actor)) = (wire_op(&ev.op), wire_actor(&ev.actor_kind)) else {
            continue;
        };
        // El path del journal es `VPath::to_wire`, o sea texto por
        // construcción; si no lo fuera, es una fila que este binario no
        // escribió y no se le pasa a nadie.
        let Ok(path) = String::from_utf8(ev.path.clone()) else {
            continue;
        };
        let path_to = ev
            .path_to
            .as_ref()
            .and_then(|b| String::from_utf8(b.clone()).ok());
        let vpath = norte_proto::VPath::parse(&path).ok();
        let leaf = vpath
            .as_ref()
            .and_then(|v| v.file_name().map(|s| s.as_bytes().to_vec()))
            .unwrap_or_default();
        let location = mint.and_then(|(m, marker)| {
            let parent = vpath.as_ref()?.parent()?;
            let key = parent.to_wire();
            let idx = match by_parent.get(&key) {
                Some(i) => *i,
                None => {
                    // Solo para el actor humano se sube hasta el marcador;
                    // aquí el que corre es un plugin, así que nunca.
                    let s = m.mint_for(&parent, marker, false)?;
                    sessions.push(s);
                    by_parent.insert(key, sessions.len() - 1);
                    sessions.len() - 1
                }
            };
            let r = sessions[idx].as_ref();
            Some(hook_iface::LocationRef {
                token: r.token,
                prefix: r.prefix,
            })
        });
        out.push(hook_iface::Event {
            seq: u64::try_from(ev.seq).unwrap_or_default(),
            ts_ms: ev.ts_ms,
            op,
            actor,
            path,
            path_to,
            name: leaf,
            batch: ev.batch_id.and_then(|b| u64::try_from(b).ok()),
            location,
        });
    }
    (out, sessions)
}

/// Una tanda contra todos los hooks consentidos. BLOQUEANTE.
fn dispatch_batch(
    config_dir: &std::path::Path,
    runtime: &PluginRuntime,
    fuse: &mut Fuse,
    batch: &[HookEvent],
) -> Vec<PluginNotice> {
    let mut notices = Vec::new();
    let reg = match PluginRegistry::discover(config_dir) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "hooks: no se pudo leer el registro de plugins");
            return notices;
        }
    };
    for (resolved, ons) in reg.resolve_hooks() {
        let (id, _name, wasm, caps, settings) = resolved;
        if fuse.is_disabled(&id) {
            continue;
        }
        let mint = caps
            .location
            .granted()
            .then(|| LocationMint::new(norte_vfs_local::Bounds::default()));
        let (events, _sessions) = to_wire_events(
            batch,
            &ons,
            mint.as_ref()
                .map(|m| (m, caps.location_root_marker.as_deref())),
        );
        if events.is_empty() {
            continue;
        }
        let host: Option<Arc<dyn norte_plugin_host::LocationHost>> = mint
            .as_ref()
            .map(|m| Arc::clone(m) as Arc<dyn norte_plugin_host::LocationHost>);
        let outcome = runtime
            .instantiate_hook_with_location(&wasm, caps, host)
            .map_err(|e| e.to_string())
            .and_then(|mut inst| {
                inst.set_settings(settings);
                inst.on_events(&events).map_err(|e| e.to_string())
            });
        match outcome {
            Ok(Ok(effects)) => {
                fuse.record_ok(&id);
                for eff in effects {
                    match eff {
                        hook_iface::Effect::Notify(text) => notices.push(PluginNotice {
                            plugin_id: id.clone(),
                            kind: "notify".to_owned(),
                            text: Some(guest_reason(&text)),
                        }),
                    }
                }
            }
            Ok(Err(frase)) => {
                tracing::warn!(plugin = %id, reason = %guest_reason(&frase), "hook: el guest rehusó");
                if fuse.record_failure(&id) {
                    notices.push(disabled_notice(&id));
                }
            }
            Err(e) => {
                tracing::warn!(plugin = %id, error = %e, "hook: fallo al ejecutar");
                if fuse.record_failure(&id) {
                    notices.push(disabled_notice(&id));
                }
            }
        }
    }
    notices
}

fn disabled_notice(id: &str) -> PluginNotice {
    tracing::warn!(
        plugin = %id,
        failures = HOOK_FUSE_FAILURES,
        "hooks apagados hasta que el plugin se reactive"
    );
    PluginNotice {
        plugin_id: id.to_owned(),
        kind: "hooks-disabled".to_owned(),
        text: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(seq: i64, op: &str, path: &str) -> HookEvent {
        HookEvent {
            seq,
            ts_ms: 1_726_000_000_000,
            op: op.to_owned(),
            actor_kind: "user".to_owned(),
            path: path.as_bytes().to_vec(),
            path_to: None,
            batch_id: Some(7),
        }
    }

    #[test]
    fn el_fusible_apaga_al_tercer_fallo_seguido_y_avisa_una_vez() {
        let mut f = Fuse::default();
        assert!(!f.record_failure("a"));
        assert!(!f.record_failure("a"));
        f.record_ok("a");
        assert!(!f.record_failure("a"), "el éxito puso el contador a cero");
        assert!(!f.record_failure("a"));
        assert!(f.record_failure("a"), "el tercero seguido apaga");
        assert!(f.is_disabled("a"));
        assert!(!f.record_failure("a"), "apagado no vuelve a avisar");
        assert!(!f.is_disabled("b"), "cada plugin lleva su fusible");
    }

    #[test]
    fn el_nombre_del_evento_sale_de_la_op_del_journal() {
        assert_eq!(event_name_for("created"), Some("after-created"));
        assert_eq!(event_name_for("mode_changed"), Some("after-mode-changed"));
        assert_eq!(event_name_for("teleported"), None);
        for e in HOOK_EVENTS {
            assert!(e.starts_with("after-"), "{e}: solo hay after-*");
        }
    }

    #[test]
    fn los_eventos_se_filtran_por_lo_que_el_manifiesto_pide() {
        let batch = vec![
            ev(1, "created", "file:///a/b.txt"),
            ev(2, "renamed", "file:///a/c.txt"),
            ev(3, "vanished", "file:///a/d.txt"),
        ];
        let (out, sessions) = to_wire_events(&batch, &["after-renamed".to_owned()], None);
        assert!(sessions.is_empty(), "sin location no se acuña nada");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].seq, 2);
        assert_eq!(out[0].path, "file:///a/c.txt");
        assert_eq!(out[0].name, b"c.txt".to_vec());
        assert_eq!(out[0].batch, Some(7));
        assert!(out[0].location.is_none());
        assert!(matches!(out[0].op, hook_iface::Op::Renamed));
    }

    #[test]
    fn una_sesion_de_ubicacion_por_directorio_padre() {
        let dir = tempfile::tempdir().expect("tempdir");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).expect("sub");
        let root = norte_vfs_local::vpath_from_native(dir.path()).expect("vpath");
        let a = format!("{}/a.txt", root.to_wire());
        let b = format!("{}/b.txt", root.to_wire());
        let c = format!("{}/sub/c.txt", root.to_wire());
        let batch = vec![
            ev(1, "created", &a),
            ev(2, "created", &b),
            ev(3, "created", &c),
        ];
        let mint = LocationMint::with_protected(vec![], norte_vfs_local::Bounds::default());
        let (out, sessions) =
            to_wire_events(&batch, &["after-created".to_owned()], Some((&mint, None)));
        assert_eq!(out.len(), 3);
        assert_eq!(sessions.len(), 2, "dos padres distintos, dos sesiones");
        let t = |i: usize| out[i].location.as_ref().map(|l| l.token.clone());
        assert_eq!(t(0), t(1), "el mismo padre comparte token");
        assert_ne!(t(0), t(2));
    }
}
