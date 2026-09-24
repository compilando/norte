//! The ENTIRE surface the webview can call. Four commands.
//!
//! There is no `rpc(method, params)`, no `read_file`, no `spawn`: what the
//! webview can ask for is what [`norte_ui_host::UiAction`] expresses, and
//! serde validates that before it reaches the host (ADR 0066, decision D11).
//! A new command here is a decision, not a shortcut — that is why there is a
//! test that pins the list.

use std::sync::Arc;

use norte_ui_host::{ActionAck, BridgeEnvelope, UiAction, UiHost, dto::UiUpdate};

use crate::catalog::HostCatalog;

/// The state Tauri injects into every command.
pub struct Bridge {
    /// In an `Arc` because pumping native effects needs a `'static` handle:
    /// the folder picker ANSWERS the host (#284), and for that it has to be
    /// able to call `dispatch` from its own task.
    host: Arc<UiHost>,
    /// The startup frame, in its envelope (sequence 0).
    inicial: BridgeEnvelope<UiUpdate>,
    /// Strings and colors. REPLACEABLE: the colors come from the theme, and
    /// the theme can be changed with the window open (from its picker, and
    /// from a profile). The strings do not move — the language is fixed once
    /// per process — but the whole catalogue travels because that is what
    /// the renderer asks for, whole.
    catalog: std::sync::RwLock<Arc<HostCatalog>>,
    /// The language it was built with, so it can be rebuilt the same way.
    lang: norte_i18n::Lang,
}

impl Bridge {
    /// Mounts the bridge over an already-started host.
    #[must_use]
    pub fn new(
        host: UiHost,
        snapshot: norte_ui_host::ViewSnapshot,
        catalog: HostCatalog,
        lang: norte_i18n::Lang,
    ) -> Self {
        let inicial = BridgeEnvelope::new(
            host.instance().clone(),
            0,
            UiUpdate::Snapshot(Box::new(snapshot)),
        );
        Self {
            host: Arc::new(host),
            inicial,
            catalog: std::sync::RwLock::new(Arc::new(catalog)),
            lang,
        }
    }

    /// Rebuilds the catalogue with the theme that is now set.
    ///
    /// A name that does not resolve does NOT leave the window without colors:
    /// the previous catalogue stays. Returns whether anything changed, so
    /// whoever notifies the renderer does not send it off to repaint for
    /// nothing.
    /// **May read a file**: `[ui] theme` is a preset or a PATH (ADR 0020), so
    /// whoever calls it decides where it runs — the native-effects pump sends
    /// it to `spawn_blocking` when it is not a preset. It used to only look
    /// at presets, and a profile with `theme = "…/mine.toml"` silently ended
    /// up without new colors.
    pub fn cambiar_tema(&self, name: &str) -> bool {
        let Ok(theme) = norte_frontend::theme::resolve_theme(Some(name)) else {
            return false;
        };
        let mut updated = crate::catalog::catalogo(self.host.instance(), self.lang, &theme);
        // The appearance is PRESERVED: this path changes colors, and rebuilding
        // the catalogue from scratch would reset the fonts to the system's
        // without anyone asking for that. It is the same catalogue with a
        // different theme.
        updated.appearance = match self.catalog.read() {
            Ok(guard) => guard.appearance.clone(),
            Err(poisoned) => poisoned.into_inner().appearance.clone(),
        };
        // A poisoned lock means another thread panicked WHILE holding the
        // catalogue. We carry on: what is inside is a whole, valid `Arc`, and
        // leaving the window unable to change theme over that would be worse.
        match self.catalog.write() {
            Ok(mut guard) => *guard = Arc::new(updated),
            Err(poisoned) => *poisoned.into_inner() = Arc::new(updated),
        }
        true
    }

    /// The host underneath (for pumping and shutdown).
    #[must_use]
    pub fn host(&self) -> &UiHost {
        &self.host
    }

    /// The same one, shareable: the native-effects pump needs it, since it
    /// lives in its own task and answers the host.
    #[must_use]
    pub fn host_compartido(&self) -> Arc<UiHost> {
        Arc::clone(&self.host)
    }

    /// The startup frame. It is sequence 0 and there is exactly one: the
    /// renderer does not start by asking for state, it already has it.
    #[must_use]
    pub fn initial_snapshot(&self) -> BridgeEnvelope<UiUpdate> {
        self.inicial.clone()
    }

    /// Applies an action from the renderer.
    ///
    /// # Errors
    /// If the host is no longer there, and says so: a renderer that does not
    /// find out the host died is left painting a frozen screen.
    pub async fn dispatch(&self, action: UiAction) -> Result<ActionAck, String> {
        self.host.dispatch(action).await.map_err(|e| e.to_string())
    }

    /// Asks for a new frame: the renderer lost track of the sequence.
    ///
    /// # Errors
    /// As [`Bridge::dispatch`].
    pub async fn request_snapshot(&self) -> Result<ActionAck, String> {
        self.dispatch(UiAction::Resync).await
    }

    /// Strings and colors, already resolved in Rust.
    #[must_use]
    pub fn catalog(&self) -> Arc<HostCatalog> {
        match self.catalog.read() {
            Ok(guard) => Arc::clone(&guard),
            // See `cambiar_tema`: what is inside is still a whole catalogue,
            // and ending up without strings is worse than carrying on.
            Err(poisoned) => Arc::clone(&poisoned.into_inner()),
        }
    }

    /// The bytes of the image the viewer has open, if any.
    ///
    /// Separate from the frame ON PURPOSE (ADR 0069): eight megabytes in the
    /// patch stream is a message that gets resent whole on every `Resync`.
    ///
    /// No PATH. The renderer does not name files: it is served the image the
    /// host decided to open, already validated against the caps — format by
    /// magic bytes, declared dimensions against the budget, size — never the
    /// one someone asks for.
    ///
    /// # Errors
    /// The reason, already as text, if the host is not there.
    pub async fn image_bytes(&self) -> Result<Vec<u8>, String> {
        self.host
            .image_bytes()
            .await
            .map(|b| b.map(|a| a.as_slice().to_vec()).unwrap_or_default())
            .map_err(|e| e.to_string())
    }
}

/// What the process has: a live bridge, or the reason it does not.
///
/// Starting without a daemon is NOT a window that fails to open: it is a
/// window that says what happened. That is why the failure is a state and not
/// an `exit`, and why every command has to know how to answer it.
pub enum AppState {
    /// Everything mounted.
    Ready(Box<Bridge>),
    /// Could not start; this is what gets shown.
    Failed(String),
}

impl AppState {
    /// The bridge, or the reason.
    ///
    /// # Errors
    /// The startup message, already ready to paint.
    pub fn bridge(&self) -> Result<&Bridge, String> {
        match self {
            Self::Ready(b) => Ok(b),
            Self::Failed(e) => Err(e.clone()),
        }
    }
}

/// The names of the commands the binary exposes, in order.
///
/// Exists so the surface is a LIST you read, not something you have to
/// deduce from a macro: adding one has to be visible in the diff.
pub const COMANDOS: &[&str] = &[
    "initial_snapshot",
    "dispatch",
    "request_snapshot",
    "catalog",
    "image_bytes",
    "window_control",
];

/// What the window's own title bar can ask of ITS window (ADR 0136).
///
/// A closed vocabulary instead of Tauri's `core:window:*` permissions: those
/// are granted to the whole webview and for any window, and this one's
/// capability grants none of them (D11). This way the door is a binary
/// command that only acts on the window that calls it, and only with its own
/// bar in place.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WindowVerb {
    /// Minimize.
    Minimize,
    /// Maximize, or restore if already maximized.
    ToggleMaximize,
    /// Close, by the same path as the desktop's X: `[ui] confirm_quit` still
    /// asks. It has to stay `Window::close()`, which emits `CloseRequested`;
    /// `destroy()` would skip the question and the session save
    /// (`la_puerta_de_la_ventana_es_estrecha` watches for that).
    Close,
    /// Start dragging the window with whichever button is pressed.
    Drag,
}

#[cfg(test)]
pub(crate) mod tests_soporte {
    //! A host against a REAL daemon, for this crate's tests.
    //!
    //! There is no screen and no Node anywhere: what is being tested is the
    //! adapter, and the adapter needs neither.

    use std::sync::Arc;
    use std::time::Duration;

    use norte_client::RemoteBackend;
    use norte_core::Engine;
    use norte_core::daemon::{Daemon, DaemonConfig};
    use norte_proto::VPath;
    use norte_proto::methods::ClientInfo;
    use norte_testkit::MemProvider;
    use norte_ui_host::{UiHost, UiHostOptions, ViewSnapshot};
    use norte_vfs::Provider;

    /// Brings up daemon + host and returns the host with its first frame.
    ///
    /// The `TempDir` is deliberately leaked (not `Box::leak`; it is kept
    /// alive in a `static`-like way): a test that deletes the socket
    /// mid-pump measures something else.
    pub async fn host_de_prueba() -> (UiHost, ViewSnapshot) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mem = Arc::new(MemProvider::new());
        let vp = |w: &str| VPath::parse(w).expect("test wire");
        mem.mkdir(&vp("mem:///casa")).await.expect("mkdir");
        mem.mkdir(&vp("mem:///casa/docs")).await.expect("mkdir");
        let socket = dir.path().join("d.sock");
        let engine = Arc::new(Engine::new());
        engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
        let d = Daemon::bind(
            engine,
            DaemonConfig {
                socket_path: Some(socket.clone()),
                idle_timeout: None,
                listing_ttl: Duration::from_mins(2),
                plugins_dir: None,
                state_dir: Some(dir.path().to_path_buf()),
            },
        )
        .await
        .expect("bind");
        let run = tokio::spawn(d.run());
        let backend = RemoteBackend::connect(
            socket,
            None,
            ClientInfo {
                name: "gui-test".to_owned(),
                version: "0.0.0".to_owned(),
            },
        )
        .await
        .expect("connects");
        let out = UiHost::start(UiHostOptions {
            backend: Arc::new(backend),
            initial_dir: vp("mem:///casa"),
            initial_dir_pedido: false,
            attach: false,
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            keymap_viewer: norte_ui_host::keys::keymap_visor_de_preset("orthodox").expect("preset"),
            keymap_dialog: norte_ui_host::keys::keymap_dialogo_de_preset("orthodox")
                .expect("preset"),
            layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
            viewport: (120, 40),
            settings: norte_ui_host::ajustes_por_defecto(),
            paths: norte_ui_host::settings::HostPaths::default(),
            theme: norte_ui_host::pickers::HostTheme::default(),
            user_layouts: Vec::new(),
            profile: None,
            columns: norte_ui_host::columnas_por_defecto(),
            effects: norte_ui_host::commands::Efectos::Completo,
            log_ring: None,
        })
        .await
        .expect("starts");
        // The daemon and its directory live as long as the test process does.
        std::mem::forget((dir, run));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The startup frame is sequence 0 and comes in an envelope of THIS
    /// contract version.
    #[tokio::test]
    async fn startup_frame_is_sequence_zero() {
        let (host, snap) = tests_soporte::host_de_prueba().await;
        let cat = crate::catalog::catalogo(
            host.instance(),
            norte_i18n::Lang::Es,
            &norte_theme::Theme::preset_default(),
        );
        let b = Bridge::new(host, snap, cat, norte_i18n::Lang::Es);
        let env = b.initial_snapshot();
        assert_eq!(env.sequence, 0);
        assert!(env.is_supported(), "the envelope is the version we speak");
        assert!(matches!(env.payload, UiUpdate::Snapshot(_)));
    }

    /// An action from the renderer reaches the host and comes back with its
    /// acknowledgment.
    #[tokio::test]
    async fn an_action_goes_and_comes_back() {
        let (host, snap) = tests_soporte::host_de_prueba().await;
        let cat = crate::catalog::catalogo(
            host.instance(),
            norte_i18n::Lang::Es,
            &norte_theme::Theme::preset_default(),
        );
        let b = Bridge::new(host, snap, cat, norte_i18n::Lang::Es);
        let ack = b
            .dispatch(UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await
            .expect("host is alive");
        assert!(matches!(ack, ActionAck::Applied { .. }));
    }

    /// An action the renderer makes up is NOT interpreted: serde rejects it
    /// at the boundary, before the host ever sees it.
    #[test]
    fn a_made_up_action_does_not_cross() {
        let json = serde_json::json!({ "action": "rm_rf", "path": "/" });
        assert!(serde_json::from_value::<UiAction>(json).is_err());
    }

    /// And an action with an extra field does not sneak in as something else
    /// either.
    #[test]
    fn an_action_with_a_path_inside_does_not_sneak_in() {
        let json = serde_json::json!({
            "action": "activate",
            "slot_id": 1,
            "key": 3,
            "generation": 4,
            "path": "/etc/passwd"
        });
        // The extra field is ignored: what acts is the opaque key, and a path
        // slipped in by the renderer does not reach anywhere.
        let a: UiAction = serde_json::from_value(json).expect("the action is valid");
        assert_eq!(
            a,
            UiAction::Activate {
                slot_id: 1,
                key: norte_ui_host::RowKey(3),
                generation: 4
            }
        );
    }
}
