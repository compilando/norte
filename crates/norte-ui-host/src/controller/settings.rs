//! La vista de ajustes: enseñarlos, girarlos, pedir un valor y escribirlo.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

/// Lo que vuelve de escribir un ajuste, FUERA del actor.
///
/// La escritura y la relectura van juntas en el mismo `spawn_blocking`: lo
/// que la barra dice al final —«guardado» y qué no se aplicó— necesita las
/// dos, y dos viajes por el buzón serían dos estados intermedios que nadie
/// quiere ver.
pub(super) struct AjusteEscrito {
    /// Cómo se llama la entrada, ya traducido, para el mensaje.
    pub(super) nombre: String,
    /// El valor nuevo como texto, para el mensaje.
    pub(super) valor: String,
    /// `Err` es la clave del motivo por el que NO se escribió. `Ok(None)` es
    /// que se escribió pero la relectura falló: el fichero está bien —lo
    /// acaba de escribir `persist_set`—, así que se dice «guardado» y la
    /// ventana sigue con la configuración que tenía hasta reiniciar.
    pub(super) resultado: Result<Option<norte_frontend::config::FrontendConfig>, &'static str>,
}

impl Estado {
    /// Abre los ajustes.
    ///
    /// Las filas se construyen AQUÍ y se congelan, como las de la paleta y
    /// por el mismo motivo: `build_rows` resuelve el valor efectivo de cada
    /// entrada y formatea dos cadenas Fluent por fila. La configuración es la
    /// que la ventana tiene PUESTA, que tras un cambio de perfil o un ajuste
    /// escrito ya no es la del arranque.
    pub(super) fn abrir_ajustes(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        self.ajustes = Some(crate::settings::Ajustes::abrir(
            &self.config,
            &self.paths,
            self.lang,
        ));
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un click en una fila de los ajustes: solo mueve el cursor.
    pub(super) fn elegir_ajuste(&mut self, row: u32) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        a.senalar(row as usize);
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Un doble click en una fila: la señala y la activa, que es lo que hace
    /// Enter. El mismo camino y no otro: el ratón es una segunda puerta a la
    /// misma máquina, no una segunda máquina.
    pub(super) fn activar_ajuste_por_raton(
        &mut self,
        row: u32,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        a.senalar(row as usize);
        self.activar_ajuste(buzon)
    }

    /// La proyección de los ajustes.
    pub(super) fn vista_ajustes(&self) -> Option<crate::dto::SettingsView> {
        Some(self.ajustes.as_ref()?.vista(self.lang))
    }

    /// Las teclas mientras los ajustes están abiertos.
    ///
    /// FIJAS, como las de la paleta y la ayuda: el catálogo no tiene
    /// comandos para «bajar por esta lista». `enter` activa la fila: gira lo
    /// que gira, y pide en un diálogo lo que se teclea.
    pub(super) fn tecla_en_ajustes(
        &mut self,
        k: &crate::keys::KeyInput,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        /// Cuántas filas mueve una página.
        const PAGINA: i64 = 10;
        if self.ajustes.is_none() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        let verbo = match k.key.as_str() {
            "Home" | "home" | "End" | "end" => None,
            _ => self.verbo_de_dialogo(k),
        };
        if verbo.as_deref() == Some("dialog.confirm") {
            return self.activar_ajuste(buzon);
        }
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        match (verbo.as_deref(), k.key.as_str()) {
            (Some("dialog.cancel"), _) => self.ajustes = None,
            (Some("dialog.down"), _) => a.mover(1),
            (Some("dialog.up"), _) => a.mover(-1),
            (Some("dialog.page-down"), _) => a.mover(PAGINA),
            (Some("dialog.page-up"), _) => a.mover(-PAGINA),
            (_, "Home" | "home") => a.mover(i64::MIN / 2),
            (_, "End" | "end") => a.mover(i64::MAX / 2),
            _ => return (self.aplicada(), Vec::new()),
        }
        let cambio = ViewChange::Settings {
            settings: self.vista_ajustes(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Activa la fila del cursor: lo que gira se escribe ya; lo que se
    /// teclea se pide en un diálogo.
    ///
    /// Las listas de temas y presets se resuelven AQUÍ y vivas, como en el
    /// terminal: el tema efectivo puede haber cambiado en caliente.
    fn activar_ajuste(
        &mut self,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Con un diálogo delante no se activa nada: el teclado ya lo ordena
        // así (el diálogo recibe la tecla antes), y el ratón tiene que
        // hacer lo mismo o un doble clic con el prompt del valor abierto
        // escribiría —o apilaría un segundo prompt— por detrás de él.
        if !self.dialogos.is_empty() {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        }
        let Some(a) = self.ajustes.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        let temas: Vec<String> = norte_theme::preset_names()
            .into_iter()
            .map(String::from)
            .collect();
        match a.activar(&temas, norte_frontend::keymap::presets::NAMES) {
            crate::settings::Activacion::Nada => (self.aplicada(), Vec::new()),
            crate::settings::Activacion::Escribir(write) => {
                let cambio = ViewChange::Settings {
                    settings: self.vista_ajustes(),
                };
                let mut salidas = vec![self.parche(vec![cambio])];
                let (motivo, partes) = self.escribir_ajuste(*write, buzon);
                salidas.extend(partes);
                match motivo {
                    Some(reason_key) => (
                        ActionAck::Unavailable {
                            reason_key: reason_key.to_owned(),
                        },
                        salidas,
                    ),
                    None => (self.aplicada(), salidas),
                }
            }
            crate::settings::Activacion::PedirTexto {
                nombre,
                actual,
                fila,
            } => self.pedir_valor_de_ajuste(&nombre, actual, fila),
        }
    }

    /// Pide el valor de una entrada de texto: el diálogo de un campo, con el
    /// valor actual dentro, y el nombre de la entrada como cuerpo.
    ///
    /// Es la forma de la ventana de hacer lo que el terminal hace tecleando
    /// en la fila: su campo es nativo y el texto vuelve entero al confirmar.
    fn pedir_valor_de_ajuste(
        &mut self,
        nombre: &str,
        actual: String,
        fila: usize,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        // El valor actual sale de un `norte.toml` que puede ser el del
        // proyecto: se pinta enmascarado, y lo que se edita es lo real.
        let (pintable, hostil) = norte_frontend::display_name(actual.as_bytes());
        let vista = DialogView {
            id,
            title_key: "modal-setting-edit".to_owned(),
            destination: None,
            subject: None,
            asker: None,
            deadline: None,
            deadline_at_ms: None,
            body: vec![crate::dto::DialogLine {
                text: clamp_display(nombre.to_owned()),
                hostile: false,
            }],
            overflow_note: String::new(),
            overflow_hostile: false,
            choices: vec![
                DialogChoice {
                    id: "confirm".to_owned(),
                    label_key: "dialog-confirm".to_owned(),
                    destructive: false,
                },
                DialogChoice {
                    id: "cancel".to_owned(),
                    label_key: "dialog-cancel".to_owned(),
                    destructive: false,
                },
            ],
            input: Some(clamp_display(pintable)),
            input_hostile: hostil,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        self.dialogos.push(Dialogo {
            id,
            vista,
            tecleado: Tecleado::Texto(actual),
            reconocido: true,
            al_confirmar: Some(Pendiente::EditarAjuste { fila }),
        });
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// El diálogo trajo el valor de la fila `fila`: se valida con el editor
    /// compartido y, si vale, se escribe.
    ///
    /// Un rechazo se dice en la barra CON sus números —«entre 8 y 32»— y
    /// vuelve al acuse por su clave, sin escribir nada.
    pub(super) fn confirmar_valor_de_ajuste(
        &mut self,
        fila: usize,
        texto: &str,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(a) = self.ajustes.as_mut() else {
            // Los ajustes se cerraron con el diálogo delante: no hay fila a
            // la que volver, y escribir «a ciegas» sería escribir otra cosa.
            return (
                Some("host-settings-closed"),
                self.decir("host-settings-closed"),
            );
        };
        match a.confirmar_texto(fila, texto) {
            Ok(write) => {
                let cambio = ViewChange::Settings {
                    settings: self.vista_ajustes(),
                };
                let mut salidas = vec![self.parche(vec![cambio])];
                let (motivo, partes) = self.escribir_ajuste(write, buzon);
                salidas.extend(partes);
                (motivo, salidas)
            }
            Err(e) => {
                // Con los números: la clave sola no dice entre qué y qué. Y
                // con el idioma del HOST, que es el de la barra.
                let (clave, texto) = match e {
                    norte_frontend::settings::SettingsEditError::NotAnInt => (
                        "msg-settings-invalid-int",
                        norte_i18n::t_in(self.lang, "msg-settings-invalid-int"),
                    ),
                    norte_frontend::settings::SettingsEditError::OutOfRange { min, max } => (
                        "msg-settings-invalid-range",
                        norte_i18n::ta_in(
                            self.lang,
                            "msg-settings-invalid-range",
                            &[("min", &min.to_string()), ("max", &max.to_string())],
                        ),
                    ),
                };
                self.status.message = Some(clamp_display(texto));
                let parche = self.parche(vec![ViewChange::Status(self.status.clone())]);
                (Some(clave), vec![parche])
            }
        }
    }

    /// Escribe `write` en la capa que esta ventana escribe y relee la
    /// configuración, FUERA del actor.
    ///
    /// El actor es el único escritor del estado y esto es I/O con un lock
    /// entre procesos detrás (`persist_set` bloquea mientras otro norte
    /// escribe): hacerlo aquí congelaría la ventana entera. Vuelve por el
    /// buzón como el tema y los favoritos.
    ///
    /// La capa es la del PERFIL activo si lo hay, y la del usuario si no
    /// ([`Self::dir_de_escritura`]): un ajuste escrito abajo que el perfil
    /// también fija queda tapado — guardado y sin efecto (ADR 0079).
    fn escribir_ajuste(
        &mut self,
        write: norte_frontend::settings::PendingWrite,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (Option<&'static str>, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(dir) = self.dir_de_escritura() else {
            return (Some("host-no-config-dir"), self.decir("host-no-config-dir"));
        };
        let capas = self.capas_actuales();
        let buzon = buzon.clone();
        let norte_frontend::settings::PendingWrite {
            section,
            key,
            value,
            name,
            display,
        } = write;
        tokio::task::spawn_blocking(move || {
            let resultado = match norte_config::persist_set(&dir, section, &key, value) {
                // El error NO viaja: puede llevar la ruta del fichero, y lo
                // que la barra dice sale del catálogo (#73). La categoría
                // basta para saber qué pasó.
                Err(e) => Err(clave_de_io(&e)),
                Ok(_) => Ok(norte_frontend::config::load(&capas).ok()),
            };
            let hecho = AjusteEscrito {
                nombre: name,
                valor: display,
                resultado,
            };
            let _ = buzon.blocking_send(Mensaje::Fondo(Box::new(Fondo::AjusteEscrito(Box::new(
                hecho,
            )))));
        });
        (None, Vec::new())
    }

    /// El ajuste ya está (o no) en el fichero: se dice, y la configuración
    /// releída se aplica por el mismo camino que un cambio de perfil.
    ///
    /// Si los ajustes siguen abiertos, sus filas se rehacen sobre lo releído:
    /// la fila giró optimista al activarla, y esto la deja diciendo lo que
    /// el fichero dice. Termina en una FOTO y no en un parche porque el tema
    /// y el keymap mueven la pantalla entera.
    pub(super) fn ajuste_escrito(
        &mut self,
        hecho: AjusteEscrito,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let AjusteEscrito {
            nombre,
            valor,
            resultado,
        } = hecho;
        let cfg = match resultado {
            Err(clave) => {
                // La fila optimista MENTÍA: se rehace sobre lo que hay puesto.
                if let Some(a) = self.ajustes.as_mut() {
                    a.refrescar(&self.config, self.lang);
                }
                let mut salidas = self.decir(clave);
                let cambio = ViewChange::Settings {
                    settings: self.vista_ajustes(),
                };
                salidas.push(self.parche(vec![cambio]));
                return salidas;
            }
            Ok(None) => {
                tracing::warn!("el ajuste se escribió pero la configuración no se pudo releer");
                None
            }
            Ok(Some(cfg)) => Some(cfg),
        };
        let fuera = cfg.map_or_else(Vec::new, |cfg| self.aplicar_config(cfg, backend, buzon));
        // Lo que el fichero del perfil activo trae y no se entiende sigue
        // ahí tras releer: se deja rastro, como al cambiar de perfil. Sin el
        // mensaje de la barra, que aquí la ocupa el «guardado» y el perfil
        // ya lo dijo al ponerse.
        for aviso in &self.config.common.profile_warnings {
            tracing::warn!(motivo = %aviso, "línea del perfil ignorada");
        }
        if let Some(a) = self.ajustes.as_mut() {
            a.refrescar(&self.config, self.lang);
        }
        self.status.message = Some(clamp_display(if fuera.is_empty() {
            norte_i18n::ta_in(
                self.lang,
                "msg-settings-saved",
                &[("name", &nombre), ("value", &valor)],
            )
        } else {
            norte_i18n::ta_in(
                self.lang,
                "msg-settings-saved-restart",
                &[("name", &nombre), ("value", &valor)],
            )
        }));
        let snap = self.snapshot();
        vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
    }
}
