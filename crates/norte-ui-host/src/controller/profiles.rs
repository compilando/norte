//! Perfiles de configuración: elegirlos, aplicarlos y guardarlos.
//!
//! Parte de `controller`: son métodos de `Estado`, movidos aquí sin
//! tocarlos (ADR 0086). El único escritor sigue siendo el actor.

// Estos módulos son el mismo `impl Estado` partido en trozos, así que usan
// los mismos imports que el padre. Enumerarlos aquí sería una lista de
// cuarenta líneas por fichero, en 32 ficheros, que se desincroniza en cuanto
// el padre importa algo — `super::*` la sigue sola.
#[allow(clippy::wildcard_imports)]
use super::*;

impl Estado {
    /// El perfil cargó (o no): se aplica todo lo que se puede aplicar sin
    /// reiniciar, y se DICE lo que no.
    ///
    /// El orden importa. Primero el tema, que es lo que se ve; después el
    /// keymap entero, las columnas y los favoritos; y al final la disposición
    /// que el perfil nombre, porque cambia los huecos y todo lo anterior tiene
    /// que estar puesto cuando se re-listen.
    ///
    /// Un perfil que NO carga no cambia nada: se sigue donde estabas y se dice
    /// por qué (ADR 0079, D7).
    pub(super) fn aplicar_perfil(
        &mut self,
        nombre: &std::ffi::OsStr,
        cargada: Result<norte_frontend::config::FrontendConfig, &'static str>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let cfg = match cargada {
            Ok(c) => c,
            Err(clave) => return self.decir(clave),
        };
        let antes = self.config.common.clone();
        self.perfil_activo = Some(nombre.to_os_string());
        self.selector_perfil = None;

        // El TEMA. Se pone por nombre, y el aviso al que hospeda sale de aquí:
        // es la mitad de para lo que existe un perfil.
        if let Some(tema) = cfg.common.ui_theme.clone() {
            self.aplicar_tema(&tema, buzon);
        }
        // El KEYMAP entero, con las capas del perfil dentro. Si no se puede
        // construir se queda el que había: un perfil con un `keymap.toml`
        // roto no puede dejar la ventana sin teclas.
        if let Ok(browse) = crate::keys::keymap_de_preset_con_capas(
            &cfg.common.preset,
            &cfg.keymap_layers,
            self.efectos,
        ) {
            self.efectivo = browse.clone();
            self.resolver = Resolver::new(browse);
        }
        if let Ok(visor) =
            crate::keys::keymap_visor_de_preset_con_capas(&cfg.common.preset, &cfg.keymap_layers)
        {
            self.efectivo_visor = visor.clone();
            self.resolver_visor = Resolver::new(visor);
        }
        if let Ok(dialogo) =
            crate::keys::keymap_dialogo_de_preset_con_capas(&cfg.common.preset, &cfg.keymap_layers)
        {
            self.resolver_dialogo = Resolver::new(dialogo);
        }
        // Columnas y favoritos.
        self.columnas = norte_frontend::columns::ColumnsSettings::resolve(&cfg.common.ui_columns);
        self.config = cfg;
        self.sembrar_sitios();
        // Y la DISPOSICIÓN que el perfil nombre, si nombra otra: es lo que
        // hace que un perfil sea «otra pantalla» y no solo otros colores.
        let mut cambios = Vec::new();
        if antes.ui_layout != self.config.common.ui_layout
            && let Some(nombre) = self.config.common.ui_layout.clone()
            && let Ok(arbol) = norte_frontend::layout::presets::tree(&nombre)
        {
            // Lo que devuelve se DESCARTA: al final de esto sale una foto
            // completa, y mandar dos seguidas es mandar la primera para nada.
            let _ = self.aplicar_disposicion(arbol, backend, buzon);
        }
        // Y `[profile.start]`: dónde abre cada hueco del que la sesión no sabe
        // nada. Es lo que hace útil un perfil recién creado o uno que llega de
        // otra máquina — sin esto, entrar en «trabajo» dejaba los dos paneles
        // donde estaban y el perfil solo cambiaba los colores.
        //
        // Va DESPUÉS de la disposición porque el hueco tiene que existir para
        // poder sembrarlo, y por `navegar_hueco` porque ahí ya puede haber una
        // petición en vuelo del listado anterior: un testigo nuevo la releva,
        // y poner el dir a mano dejaría aterrizar la vieja encima.
        //
        // `Seed` y no `Record`: sembrar no es un paso que el lector anduvo, y
        // meter en el «atrás» de este perfil el directorio del anterior es
        // ofrecer una vuelta a un sitio del que nunca se vino. Es además lo
        // que hace el terminal, que siembra construyendo el pane de cero.
        //
        // Los parches que `navegar_hueco` devuelve se DESCARTAN, igual que los
        // de la disposición y por lo mismo: esto acaba en una foto entera. Se
        // gastan números de secuencia que el renderer no llega a ver, y no es
        // un agujero — una foto cierra cualquier hueco de la secuencia, que es
        // justo para lo que existe.
        for (id, destino) in self.siembra_de_perfil() {
            let _ = self.navegar_hueco(id, &destino, Trail::Seed, backend, buzon);
        }
        // Lo que NO se puede aplicar sin reiniciar se dice por su nombre: un
        // cambio que se callara esto sería un cambio que miente (D8).
        let fuera = fuera_de_alcance_en_caliente(&antes, &self.config.common);
        // Y lo que el FICHERO del perfil trae y no se entiende, que gana a los
        // otros dos mensajes: «no se pudo aplicar en caliente» describe un
        // límite de este proceso, y esto describe líneas que no van a hacer
        // nada nunca. Callarlas es lo que convirtió `[profile.start]` en una
        // trampa — una ruta sin esquema se tiraba y el hueco abría donde le
        // parecía, sin que nada lo dijera.
        let avisos = self.config.common.profile_warnings.len();
        for aviso in &self.config.common.profile_warnings {
            tracing::warn!(motivo = %aviso, "línea del perfil ignorada");
        }
        if avisos > 0 {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-config-ignored",
                &[
                    ("profile", &nombre.to_string_lossy()),
                    ("n", &avisos.to_string()),
                ],
            )));
        } else if fuera.is_empty() {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-switched",
                &[("profile", &nombre.to_string_lossy())],
            )));
        } else {
            self.status.message = Some(clamp_display(norte_i18n::ta_in(
                self.lang,
                "msg-profile-switched-partial",
                &[
                    ("profile", &nombre.to_string_lossy()),
                    ("keys", &fuera.join(", ")),
                ],
            )));
        }
        let snap = self.snapshot();
        cambios.push(self.sobre(UiUpdate::Snapshot(Box::new(snap))));
        cambios
    }

    /// Pide la lista de perfiles, FUERA del actor.
    ///
    /// `vecino` dice qué se hace con ella cuando llegue: `None` abre el
    /// selector, `Some(hacia_delante)` salta al de al lado sin abrir nada —
    /// que es lo que quiere quien tiene dos perfiles y alterna.
    ///
    /// Leer `profiles/` es un directorio y un `norte.toml` por perfil: en el
    /// actor congelaría la ventana, y con un directorio en un NFS caído la
    /// congelaría hasta que expire el montaje (regla 2, #244).
    pub(super) fn pedir_perfiles(
        &mut self,
        vecino: Option<bool>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(dir) = self.dir_de_perfiles() else {
            return self.no_hay_perfiles();
        };
        let buzon = buzon.clone();
        tokio::task::spawn_blocking(move || {
            let perfiles = norte_frontend::config::read_profiles(&dir);
            let _ =
                buzon.blocking_send(Mensaje::Fondo(Box::new(Fondo::Perfiles(perfiles, vecino))));
        });
        (self.aplicada(), Vec::new())
    }

    /// Dónde vive `profiles/`: la capa del USUARIO.
    ///
    /// Del usuario y solo de ella, aunque haya un perfil activo: los perfiles
    /// no anidan (D1), y buscarlos dentro del perfil puesto sería inventarse
    /// una jerarquía que la ADR no tiene.
    pub(super) fn dir_de_perfiles(&self) -> Option<std::path::PathBuf> {
        use crate::settings::ConfigLayer;
        self.paths
            .config_layers
            .iter()
            .find(|(capa, _)| matches!(capa, ConfigLayer::User))
            .map(|(_, p)| p.path.clone())
    }

    /// No hay dónde buscar perfiles, y se dice.
    pub(super) fn no_hay_perfiles(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let envios = self.decir("host-no-profiles");
        (
            ActionAck::Unavailable {
                reason_key: "host-no-profiles".to_owned(),
            },
            envios,
        )
    }

    /// La lista de perfiles llegó: o se abre el selector, o se salta al
    /// vecino.
    pub(super) fn con_los_perfiles(
        &mut self,
        perfiles: Vec<norte_frontend::profile_picker::UserProfile>,
        vecino: Option<bool>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        self.gen_perfiles += 1;
        let Some(hacia_delante) = vecino else {
            self.selector_perfil = Some(norte_frontend::profile_picker::ProfilePicker::open(
                perfiles,
                self.perfil_activo.as_deref(),
            ));
            return vec![self.parche(vec![ViewChange::Profiles {
                profiles: self.vista_perfiles(),
            }])];
        };
        // Girar sobre uno solo no es un cambio: tirar y recargar la pantalla
        // para dejarla igual sería peor que no hacer nada, y decirlo es más
        // honesto que fingir que pasó algo.
        let Some(siguiente) = norte_frontend::profile_picker::next_profile(
            &perfiles,
            self.perfil_activo.as_deref(),
            hacia_delante,
        ) else {
            return self.decir("host-no-other-profile");
        };
        self.cambiar_de_perfil(&siguiente, buzon)
    }

    /// Empieza un cambio de perfil: carga su configuración FUERA del actor.
    ///
    /// El cambio no se aplica aquí. Cargar la configuración lee entre uno y
    /// cuatro ficheros por capa, y hasta que no se sabe si carga no se toca
    /// nada: un perfil roto deja al lector donde estaba (ADR 0079, D7).
    pub(super) fn cambiar_de_perfil(
        &mut self,
        nombre: &std::ffi::OsStr,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let Some(capas) = self.capas_con_perfil(nombre) else {
            return self.decir("host-no-profiles");
        };
        let nombre = nombre.to_os_string();
        let buzon = buzon.clone();
        tokio::task::spawn_blocking(move || {
            let res = norte_frontend::config::load(&capas).map_err(|_| "host-profile-broken");
            let _ = buzon.blocking_send(Mensaje::Fondo(Box::new(Fondo::PerfilCargado(
                nombre,
                Box::new(res),
            ))));
        });
        Vec::new()
    }

    /// Las capas de configuración CON el perfil puesto.
    ///
    /// Se construyen sobre las que quien arrancó el host resolvió, no mirando
    /// el entorno otra vez: la ventana lee de donde de verdad leyó (ADR 0066
    /// D14). El perfil entra por encima de la del usuario y por debajo de la
    /// del proyecto, que es su sitio (ADR 0079, D1) — y como aquí las capas
    /// vienen en precedencia ascendente, basta insertarla justo detrás de la
    /// del usuario.
    pub(super) fn capas_con_perfil(
        &self,
        nombre: &std::ffi::OsStr,
    ) -> Option<norte_config::Layers> {
        use crate::settings::ConfigLayer;
        let mut dirs = Vec::new();
        let mut puesto = false;
        for (capa, ruta) in &self.paths.config_layers {
            match capa {
                ConfigLayer::System => dirs.push((ruta.path.clone(), norte_config::Layer::System)),
                ConfigLayer::User => {
                    dirs.push((ruta.path.clone(), norte_config::Layer::User));
                    dirs.push((
                        ruta.path.join("profiles").join(nombre),
                        norte_config::Layer::Profile,
                    ));
                    puesto = true;
                }
                // La que hubiera se REEMPLAZA: cambiar de perfil no apila
                // perfiles.
                ConfigLayer::Profile => {}
                ConfigLayer::Project => {
                    dirs.push((ruta.path.clone(), norte_config::Layer::Project));
                }
            }
        }
        puesto.then_some(norte_config::Layers { dirs })
    }

    /// Abre el selector de tema, con el cursor en el vigente.
    ///
    /// Antes solo ENSEÑABA el tema activo por dentro, y no por falta de
    /// ganas: lo que hospeda a esta ventana resuelve el tema una vez al
    /// arrancar, así que no había forma de que un tema elegido se viera. Con
    /// [`NativeEffect::ThemeChanged`] la hay, y este selector es el mismo que
    /// el del terminal — presets, cursor en el que está puesto, y preview EN
    /// VIVO al moverse.
    pub(super) fn abrir_tema(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let nombres: Vec<String> = norte_theme::preset_names()
            .into_iter()
            .map(String::from)
            .collect();
        let cursor = nombres
            .iter()
            .position(|n| *n == self.tema.name)
            .unwrap_or(0);
        self.tema_elegido = Some(SeleccionDeTema {
            nombres,
            cursor,
            previo: Box::new(self.tema.clone()),
        });
        let cambio = ViewChange::Theme {
            theme: self.vista_tema(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }
}
