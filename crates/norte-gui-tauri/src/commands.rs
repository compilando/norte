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
    /// En `Arc` porque el bombeo de efectos nativos necesita un handle
    /// `'static`: el selector de carpeta le CONTESTA al host (#284), y para
    /// eso tiene que poder llamar a `dispatch` desde su propia task.
    host: Arc<UiHost>,
    /// La foto de arranque, en su sobre (secuencia 0).
    inicial: BridgeEnvelope<UiUpdate>,
    /// Textos y colores. REEMPLAZABLE: los colores salen del tema, y el tema
    /// se puede cambiar con la ventana abierta (desde su selector, y desde un
    /// perfil). Los textos no se mueven —el idioma se fija una vez por
    /// proceso— pero el catálogo viaja entero porque es lo que el renderer
    /// pide entero.
    catalog: std::sync::RwLock<Arc<HostCatalog>>,
    /// El idioma con el que se construyó, para poder reconstruirlo igual.
    lang: norte_i18n::Lang,
}

impl Bridge {
    /// Monta el puente sobre un host ya arrancado.
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

    /// Rehace el catálogo con el tema que ahora hay puesto.
    ///
    /// Un nombre que no resuelve NO deja la ventana sin colores: se queda el
    /// catálogo que había. Devuelve si cambió algo, para que quien avisa al
    /// renderer no le mande a repintar por nada.
    pub fn cambiar_tema(&self, nombre: &str) -> bool {
        let Ok(Some(tema)) = norte_theme::Theme::preset(nombre) else {
            return false;
        };
        let nuevo = crate::catalog::catalogo(self.host.instance(), self.lang, &tema);
        // Un lock envenenado significa que otro hilo panicó CON el catálogo en
        // la mano. Se sigue: lo que hay dentro es un `Arc` entero y válido, y
        // dejar la ventana sin poder cambiar de tema por eso sería peor.
        match self.catalog.write() {
            Ok(mut guard) => *guard = Arc::new(nuevo),
            Err(env) => *env.into_inner() = Arc::new(nuevo),
        }
        true
    }

    /// El host que hay debajo (para el bombeo y el apagado).
    #[must_use]
    pub fn host(&self) -> &UiHost {
        &self.host
    }

    /// El mismo, compartible: lo necesita el bombeo de efectos nativos, que
    /// vive en su propia task y le contesta al host.
    #[must_use]
    pub fn host_compartido(&self) -> Arc<UiHost> {
        Arc::clone(&self.host)
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
        match self.catalog.read() {
            Ok(guard) => Arc::clone(&guard),
            // Ver `cambiar_tema`: lo de dentro sigue siendo un catálogo
            // entero, y quedarse sin textos es peor que seguir.
            Err(env) => Arc::clone(&env.into_inner()),
        }
    }

    /// Los bytes de la imagen que el visor tiene abierta, si los hay.
    ///
    /// Aparte de la foto A PROPÓSITO (ADR 0069): ocho megas en el flujo de
    /// parches es un mensaje que se reenvía entero en cada `Resync`.
    ///
    /// Sin RUTA. El renderer no nombra ficheros: se le sirve la imagen que el
    /// host decidió abrir, ya validada contra los topes —formato por bytes
    /// mágicos, dimensiones declaradas contra el presupuesto, tamaño— y no la
    /// que alguien pida.
    ///
    /// # Errors
    /// El motivo, ya en texto, si el host no está.
    pub async fn image_bytes(&self) -> Result<Vec<u8>, String> {
        self.host
            .image_bytes()
            .await
            .map(|b| b.map(|a| a.as_slice().to_vec()).unwrap_or_default())
            .map_err(|e| e.to_string())
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
    "image_bytes",
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
        let b = Bridge::new(host, snap, cat, norte_i18n::Lang::Es);
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
        let b = Bridge::new(host, snap, cat, norte_i18n::Lang::Es);
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
            "generation": 4,
            "path": "/etc/passwd"
        });
        // Se ignora el campo sobrante: lo que actúa es la clave opaca, y una
        // ruta metida por el renderer no llega a ninguna parte.
        let a: UiAction = serde_json::from_value(json).expect("la acción es válida");
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
