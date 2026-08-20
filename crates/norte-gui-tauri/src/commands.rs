//! La superficie ENTERA que la webview puede llamar. Cuatro comandos.
//!
//! No hay un `rpc(method, params)`, ni un `read_file`, ni un `spawn`: lo que
//! la webview puede pedir es lo que [`norte_ui_host::UiAction`] expresa, y eso
//! lo valida serde antes de que llegue al host (ADR 0066, decisión D11). Un
//! comando nuevo aquí es una decisión, no un atajo — por eso hay un test que
//! clava la lista.

use std::sync::Arc;

use norte_ui_host::{ActionAck, BridgeEnvelope, UiAction, UiHost, dto::UiUpdate};

use crate::catalog::HostCatalog;

/// El estado que Tauri inyecta en cada comando.
pub struct Bridge {
    host: UiHost,
    /// La foto de arranque, en su sobre (secuencia 0).
    inicial: BridgeEnvelope<UiUpdate>,
    catalog: Arc<HostCatalog>,
}

impl Bridge {
    /// Monta el puente sobre un host ya arrancado.
    #[must_use]
    pub fn new(host: UiHost, snapshot: norte_ui_host::ViewSnapshot, catalog: HostCatalog) -> Self {
        let inicial = BridgeEnvelope::new(host.instance().clone(), 0, UiUpdate::Snapshot(snapshot));
        Self {
            host,
            inicial,
            catalog: Arc::new(catalog),
        }
    }

    /// El host que hay debajo (para el bombeo y el apagado).
    #[must_use]
    pub fn host(&self) -> &UiHost {
        &self.host
    }

    /// La foto de arranque. Es la secuencia 0 y hay exactamente una: el
    /// renderer no arranca preguntando por el estado, ya lo tiene.
    #[must_use]
    pub fn initial_snapshot(&self) -> BridgeEnvelope<UiUpdate> {
        self.inicial.clone()
    }

    /// Aplica una acción del renderer.
    ///
    /// # Errors
    /// Si el host ya no está, y se dice: un renderer que no se entera de que
    /// el host murió se queda pintando una pantalla congelada.
    pub async fn dispatch(&self, action: UiAction) -> Result<ActionAck, String> {
        self.host.dispatch(action).await.map_err(|e| e.to_string())
    }

    /// Pide una foto nueva: el renderer perdió el hilo de la secuencia.
    ///
    /// # Errors
    /// Como [`Bridge::dispatch`].
    pub async fn request_snapshot(&self) -> Result<ActionAck, String> {
        self.dispatch(UiAction::Resync).await
    }

    /// Textos y colores, ya resueltos en Rust.
    #[must_use]
    pub fn catalog(&self) -> Arc<HostCatalog> {
        Arc::clone(&self.catalog)
    }
}

/// Lo que el proceso tiene: un puente vivo, o el motivo por el que no.
///
/// Arrancar sin daemon NO es una ventana que no abre: es una ventana que
/// dice qué pasó. Por eso el fallo es un estado y no un `exit`, y por eso
/// todos los comandos tienen que saber contestarlo.
pub enum AppState {
    /// Todo montado.
    Ready(Box<Bridge>),
    /// No se pudo arrancar; esto es lo que se enseña.
    Failed(String),
}

impl AppState {
    /// El puente, o el motivo.
    ///
    /// # Errors
    /// El mensaje de arranque, ya listo para pintar.
    pub fn bridge(&self) -> Result<&Bridge, String> {
        match self {
            Self::Ready(b) => Ok(b),
            Self::Failed(e) => Err(e.clone()),
        }
    }
}

/// Los nombres de los comandos que el binario expone, en orden.
///
/// Existe para que la superficie sea una LISTA que se lee, y no algo que hay
/// que deducir de un macro: añadir uno tiene que ser visible en el diff.
pub const COMANDOS: &[&str] = &[
    "initial_snapshot",
    "dispatch",
    "request_snapshot",
    "catalog",
];

#[cfg(test)]
pub(crate) mod tests_soporte {
    //! Un host contra un daemon DE VERDAD, para los tests de este crate.
    //!
    //! No hay pantalla ni Node por ningún lado: lo que se prueba es el
    //! adaptador, y el adaptador no necesita ninguna de las dos cosas.

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

    /// Levanta daemon + host y devuelve el host con su primera foto.
    ///
    /// El `TempDir` se filtra a propósito (`Box::leak` no; se deja vivo en un
    /// `static`-like): un test que borre el socket a mitad del bombeo mide
    /// otra cosa.
    pub async fn host_de_prueba() -> (UiHost, ViewSnapshot) {
        let dir = tempfile::tempdir().expect("tempdir");
        let mem = Arc::new(MemProvider::new());
        let vp = |w: &str| VPath::parse(w).expect("wire de test");
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
        .expect("conecta");
        let out = UiHost::start(UiHostOptions {
            backend: Arc::new(backend),
            initial_dir: vp("mem:///casa"),
            locale: "es".to_owned(),
            keymap: norte_ui_host::keys::keymap_de_preset("orthodox").expect("preset"),
            layout: norte_frontend::layout::presets::tree("orthodox").expect("layout"),
            viewport: (120, 40),
            columns: norte_ui_host::columnas_por_defecto(),
        })
        .await
        .expect("arranca");
        // El daemon y su directorio viven lo que viva el proceso de test.
        std::mem::forget((dir, run));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// La foto de arranque es la secuencia 0 y viene en un sobre de ESTA
    /// versión del contrato.
    #[tokio::test]
    async fn la_foto_de_arranque_es_la_cero() {
        let (host, snap) = tests_soporte::host_de_prueba().await;
        let cat = crate::catalog::catalogo(
            host.instance(),
            norte_i18n::Lang::Es,
            &norte_theme::Theme::preset_default(),
        );
        let b = Bridge::new(host, snap, cat);
        let env = b.initial_snapshot();
        assert_eq!(env.sequence, 0);
        assert!(env.is_supported(), "el sobre es de la versión que hablamos");
        assert!(matches!(env.payload, UiUpdate::Snapshot(_)));
    }

    /// Una acción del renderer llega al host y vuelve con su acuse.
    #[tokio::test]
    async fn una_accion_va_y_vuelve() {
        let (host, snap) = tests_soporte::host_de_prueba().await;
        let cat = crate::catalog::catalogo(
            host.instance(),
            norte_i18n::Lang::Es,
            &norte_theme::Theme::preset_default(),
        );
        let b = Bridge::new(host, snap, cat);
        let ack = b
            .dispatch(UiAction::MoveCursor {
                slot_id: 1,
                delta: 1,
            })
            .await
            .expect("host vivo");
        assert!(matches!(ack, ActionAck::Applied { .. }));
    }

    /// Una acción que el renderer se invente NO se interpreta: serde la
    /// rechaza en la frontera, antes de que el host la vea.
    #[test]
    fn una_accion_inventada_no_cruza() {
        let json = serde_json::json!({ "action": "rm_rf", "path": "/" });
        assert!(serde_json::from_value::<UiAction>(json).is_err());
    }

    /// Y una acción con un campo de más tampoco cuela como otra cosa.
    #[test]
    fn una_accion_con_ruta_dentro_no_cuela() {
        let json = serde_json::json!({
            "action": "activate",
            "slot_id": 1,
            "key": 3,
            "path": "/etc/passwd"
        });
        // Se ignora el campo sobrante: lo que actúa es la clave opaca, y una
        // ruta metida por el renderer no llega a ninguna parte.
        let a: UiAction = serde_json::from_value(json).expect("la acción es válida");
        assert_eq!(
            a,
            UiAction::Activate {
                slot_id: 1,
                key: norte_ui_host::RowKey(3)
            }
        );
    }
}
