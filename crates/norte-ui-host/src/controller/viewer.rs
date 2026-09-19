//! El visor de ficheros y de imágenes.
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
    /// Las teclas mientras el visor está abierto.
    ///
    /// Resuelven con el mapa de la pantalla `viewer`, y lo que no está ligado
    /// ahí NO cae al listado: un visor abierto que dejara pasar `F8` sería un
    /// borrado con la pantalla tapada.
    pub(super) fn tecla_en_visor(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Ok(chord) = k.to_chord() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-key-unmapped".to_owned(),
                },
                Vec::new(),
            );
        };
        let Resolution::Run { command, count } = self.resolver_visor.push(chord) else {
            // Prefijo a medias, contador o nada: el visor no tiene barra de
            // estado propia todavía, así que no hay nada que pintar.
            return (self.aplicada(), Vec::new());
        };
        if command == "app.help" {
            // La ayuda es de la APLICACIÓN y no del visor, así que no está en
            // su lista de comandos —y no puede estarlo: las dos listas son
            // disjuntas a propósito—. Se atiende aquí para que `F1` con el
            // visor abierto abra la página del visor y no conteste «aquí no».
            return self.abrir_ayuda(backend, buzon);
        }
        let Some(efecto) = crate::commands::efecto_visor_de(&command, count.times()) else {
            // En el catálogo y ligado a esta pantalla, pero este host no lo
            // hace: se dice, con la misma frase que el TUI.
            let frase = norte_frontend::keymap::unavailable_message_in(
                &command,
                Availability::NotHere,
                self.lang,
            );
            self.status.message = Some(clamp_display(frase));
            let cambio = ViewChange::Status(self.status.clone());
            return (
                ActionAck::Unavailable {
                    reason_key: "cmd-not-here".to_owned(),
                },
                vec![self.parche(vec![cambio])],
            );
        };
        let alto = self.alto_del_visor();
        let Some(v) = self.visor.as_mut() else {
            return (Self::obsoleta(StaleAction::Generation), Vec::new());
        };
        // `unsigned_abs`, no `abs`: el delta de la rueda llega CRUDO del
        // renderer, y `i64::MIN.abs()` desborda —panic en debug, envuelto en
        // release—, o sea que un mensaje mal formado tumbaría el host.
        let pasos = |n: i64| usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
        match efecto {
            crate::commands::EfectoVisor::Cerrar => {
                self.visor = None;
                self.visor_en_vuelo = None;
                self.visor_token = None;
                // La imagen se SUELTA al cerrar: son megas, y un visor
                // cerrado no tiene nada que enseñar.
                self.imagen = None;
                self.miniatura = None;
            }
            crate::commands::EfectoVisor::Linea(n) if n < 0 => v.scroll_up(pasos(n)),
            crate::commands::EfectoVisor::Linea(n) => v.scroll_down(pasos(n)),
            crate::commands::EfectoVisor::Pagina(n) if n < 0 => {
                v.scroll_up(pasos(n).saturating_mul(alto));
            }
            crate::commands::EfectoVisor::Pagina(n) => {
                v.scroll_down(pasos(n).saturating_mul(alto));
            }
            crate::commands::EfectoVisor::Columna(n) if n < 0 => v.scroll_left(pasos(n)),
            crate::commands::EfectoVisor::Columna(n) => v.scroll_right(pasos(n)),
            crate::commands::EfectoVisor::Extremo { al_final: false } => v.scroll_top(),
            crate::commands::EfectoVisor::Extremo { al_final: true } => v.scroll_bottom(),
            crate::commands::EfectoVisor::Hex => v.toggle_hex(),
            crate::commands::EfectoVisor::Encoding => v.cycle_encoding(),
            crate::commands::EfectoVisor::EncodingAuto => v.reset_encoding(),
            crate::commands::EfectoVisor::Zoom { acercar: true } => v.zoom_in(),
            crate::commands::EfectoVisor::Zoom { acercar: false } => v.zoom_out(),
            crate::commands::EfectoVisor::ZoomAjustar => v.zoom_fit(),
        }
        // Un PARCHE del visor. La foto entera mandaba, por cada línea de
        // scroll, las filas visibles de todos los listados que hay debajo.
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// La RUEDA sobre el visor a pantalla completa (puente 59).
    ///
    /// Los dos ejes, porque un solo gesto los produce: la rueda a secas baja,
    /// con `shift` va de lado. Un parche del visor y no una foto, por lo mismo
    /// que las teclas: la foto entera mandaría, por cada giro, las filas
    /// visibles de todos los listados que hay debajo y que nadie ve.
    pub(super) fn desplazar_visor(
        &mut self,
        lineas: i64,
        columnas: i64,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(v) = self.visor.as_mut() else {
            return (Self::obsoleta(StaleAction::Modal), Vec::new());
        };
        // `unsigned_abs`, no `abs`: el delta de la rueda llega CRUDO del
        // renderer, y `i64::MIN.abs()` desborda —panic en debug, envuelto en
        // release—, o sea que un mensaje mal formado tumbaría el host.
        let pasos = |n: i64| usize::try_from(n.unsigned_abs()).unwrap_or(usize::MAX);
        if lineas < 0 {
            v.scroll_up(pasos(lineas));
        } else {
            v.scroll_down(pasos(lineas));
        }
        if columnas < 0 {
            v.scroll_left(pasos(columnas));
        } else {
            v.scroll_right(pasos(columnas));
        }
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        (self.aplicada(), vec![self.parche(vec![cambio])])
    }

    /// Guarda el catálogo de atributos de un esquema y repinta.
    ///
    /// Manda una FOTO y no un parche: el catálogo cambia cómo se leen celdas
    /// que ya viajaron —un modo que llegó como número y ahora es `rwx`—, y
    /// eso no es un cambio de filas, es otra lectura de todo lo que hay.
    pub(super) fn aplicar_catalogo(
        &mut self,
        scheme: String,
        catalogo: norte_proto::AttrCatalog,
    ) -> BridgeEnvelope<UiUpdate> {
        self.catalogos.insert(scheme, catalogo);
        let snap = self.snapshot();
        self.sobre(UiUpdate::Snapshot(Box::new(snap)))
    }

    /// Pide el contenido de la entrada bajo el cursor para abrir el visor.
    pub(super) fn pedir_visor(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(entrada) = self.hueco().pane.selected().cloned() else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-to-view".to_owned(),
                },
                Vec::new(),
            );
        };
        if entrada.kind == EntryKind::Dir {
            // Ver un directorio es entrar en él, y eso ya tiene su tecla.
            return (
                ActionAck::Unavailable {
                    reason_key: "host-cannot-view-dir".to_owned(),
                },
                Vec::new(),
            );
        }
        self.token += 1;
        let token = RequestToken(self.token);
        self.visor_en_vuelo = Some(token);
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let path = entrada.path.clone();
        // El ancho del visor, en celdas, para el previewer (proto 0.66.0):
        // un previewer de imagen encoge a esto. Lo que MIDIÓ el renderer la
        // última vez que pintó el cuerpo del visor (`SetViewerCols`), o el
        // viewport si todavía no lo ha pintado nunca — que se pasa por el
        // cromo, y por eso no es la primera opción.
        let columnas = Some(self.visor_columnas.unwrap_or(u32::from(self.viewport.0)));
        tokio::spawn(async move {
            // Un byte de más que el presupuesto: es lo que delata que el
            // fichero seguía. El resto NO se lee.
            let lectura = backend.read(
                path.clone(),
                Some(norte_proto::ByteRange {
                    offset: 0,
                    len: Some(VISOR_CAP + 1),
                }),
            );
            // Con plazo: un montaje colgado no puede dejar la tecla F3 sin
            // desenlace para siempre.
            let leido = match tokio::time::timeout(PLAZO_VISOR, lectura).await {
                Ok(r) => r,
                // El wire no tiene «se acabó el tiempo»; lo que hubo es una
                // lectura que no llegó, y para el usuario es lo mismo que un
                // provider que no responde.
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            // Y se le pregunta a los plugins. Un previewer que falla, que
            // tarda o que no aplica NO es un error: el visor cae a la vista
            // cruda, que es lo que el TUI ya hace. Un plugin no puede dejar
            // un fichero sin poder mirarse.
            let preview = match tokio::time::timeout(
                PLAZO_PLUGINS,
                backend.plugin_preview_styled(path.clone(), columnas),
            )
            .await
            {
                Ok(Ok(p)) => p,
                _ => None,
            };
            let _ = buzon
                .send(Mensaje::Contenido(Box::new((token, path, leido, preview))))
                .await;
        });
        (self.aplicada(), Vec::new())
    }

    /// Abre el visor con lo que se leyó.
    pub(super) fn abrir_visor(
        &mut self,
        token: RequestToken,
        path: VPath,
        leido: Result<Vec<u8>, Error>,
        preview: Option<norte_proto::methods::PluginPreviewStyled>,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_en_vuelo != Some(token) {
            // El usuario cerró el visor, pidió otro fichero o se fue a otro
            // sitio mientras esto volaba. Abrirlo ahora sería abrir una
            // ventana que nadie ha pedido — y cambiarle el teclado de mapa.
            return None;
        }
        self.visor_en_vuelo = None;
        self.visor_token = Some(token);
        // Un visor nuevo: la imagen del anterior sobra. Y hay que soltarla,
        // no solo dejar de pintarla: son megas.
        self.imagen = None;
        self.miniatura = None;
        match leido {
            Ok(mut bytes) => {
                let cap = usize::try_from(VISOR_CAP).unwrap_or(usize::MAX);
                let truncado = bytes.len() > cap;
                if truncado {
                    bytes.truncate(cap);
                }
                let ruta = path.clone();
                self.visor = Some(match preview {
                    // Un previewer aplicó: se enseña LO SUYO. Los bytes ya
                    // leídos no se tiran —hicieron falta para saber que el
                    // fichero se puede leer— pero no se pintan: pintar las
                    // dos cosas sería enseñar el mismo fichero dos veces.
                    Some(p) => norte_frontend::viewer::Viewer::with_plugin_preview_styled(
                        path,
                        p.plugin_name,
                        &p.lines,
                        p.lossy,
                    ),
                    None => norte_frontend::viewer::Viewer::new(path, bytes, truncado),
                });
                self.pedir_imagen(&ruta, token, backend, buzon);
                self.pedir_miniatura(&ruta, token, backend, buzon);
            }
            Err(e) => {
                // No se pudo leer: se DICE y no se abre un visor vacío que
                // parezca un fichero de cero bytes.
                // El texto de un error puede venir de un peer más nuevo
                // (`LimitExceeded` con un token desconocido, una huella de
                // host) y acaba en el DOM: se enmascara como cualquier otro
                // texto ajeno.
                let (pintable, _) = norte_frontend::display_name(format!("{e}").as_bytes());
                self.status.message = Some(clamp_display(pintable));
                let cambio = ViewChange::Status(self.status.clone());
                return Some(self.parche(vec![cambio]));
            }
        }
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Trae los bytes ENTEROS de la imagen, si el visor tiene una aceptada.
    ///
    /// La cabecera ya se leyó con el visor y ya dijo que sí; esto trae el
    /// resto. Si el fichero cabía en lo que se leyó no hay segundo viaje: los
    /// bytes ya están.
    ///
    /// El tope es una NEGATIVA, no un recorte. Media imagen decodificada es
    /// una imagen de otra cosa, así que un fichero por encima de
    /// [`IMAGEN_CAP`] no se pinta y se dice.
    pub(super) fn pedir_imagen(
        &mut self,
        path: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        let Some(v) = self.visor.as_ref() else {
            return;
        };
        if !matches!(Self::imagen_de(v), Ok(Some(_))) {
            return;
        }
        if !v.truncated {
            // Cabía entera en la lectura del visor: no hay nada que pedir.
            self.imagen = v.image_bytes().map(|b| std::sync::Arc::new(b.to_vec()));
            return;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let path = path.clone();
        tokio::spawn(async move {
            // Un byte de más que el tope: es lo que delata que no cabe.
            let lectura = backend.read(
                path,
                Some(norte_proto::ByteRange {
                    offset: 0,
                    len: Some(IMAGEN_CAP + 1),
                }),
            );
            let leido = match tokio::time::timeout(PLAZO_VISOR, lectura).await {
                Ok(r) => r,
                Err(_) => Err(Error::ProviderUnavailable { retryable: true }),
            };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Imagen(token, leido))))
                .await;
        });
    }

    /// Los bytes de la imagen, llegados.
    ///
    /// Se descartan si el visor ya es otro: pintar la foto anterior sobre el
    /// fichero de ahora es la misma clase de error que abrir un visor que
    /// nadie pidió.
    /// Pide a un plugin la MINIATURA del fichero del visor (ADR 0107), y
    /// solo cuando el visor no tiene imagen propia que pintar: un formato
    /// que la webview no decodifica, o una imagen que no cabe en sus topes.
    /// Con imagen propia no se molesta a nadie. El lado pedido es el alto
    /// del visor en píxeles estimados (`alto` filas × 22), acotado.
    pub(super) fn pedir_miniatura(
        &mut self,
        path: &VPath,
        token: RequestToken,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        // El techo es el del plugin-host (`THUMB_MAX_EDGE`), repetido aquí
        // porque este crate no lo conoce: acota igual en los dos lados.
        const MINIATURA_MAX_EDGE: u32 = 2048;
        let Some(v) = self.visor.as_ref() else {
            return;
        };
        if matches!(Self::imagen_de(v), Ok(Some(_))) {
            return;
        }
        let filas = u32::from(self.viewport.1).max(10);
        let max_edge = (filas * 22).clamp(128, MINIATURA_MAX_EDGE);
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let path = path.clone();
        tokio::spawn(async move {
            let thumb =
                match tokio::time::timeout(PLAZO_PLUGINS, backend.plugin_thumbnail(path, max_edge))
                    .await
                {
                    Ok(Ok(t)) => t,
                    _ => None,
                };
            let _ = buzon
                .send(Mensaje::Fondo(Box::new(Fondo::Miniatura(token, thumb))))
                .await;
        });
    }

    /// La miniatura llegó (o no): con ella, el visor la anuncia como imagen
    /// y dice de quién es; sin ella, nada cambia. Los bytes ya vienen
    /// verificados por el plugin-host (ADR 0107 decisión 3); aquí se acota
    /// el nombre del plugin, que es texto suyo.
    pub(super) fn aplicar_miniatura(
        &mut self,
        token: RequestToken,
        thumb: Option<norte_proto::methods::PluginThumbnail>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_token != Some(token) {
            return None;
        }
        let t = thumb?;
        if t.bytes.is_empty() || t.width == 0 || t.height == 0 {
            return None;
        }
        let (nombre, _) = norte_frontend::display_name(t.plugin_name.as_bytes());
        self.imagen = Some(std::sync::Arc::new(t.bytes));
        self.miniatura = Some((
            crate::dto::ImageView {
                // La etiqueta que el visor pinta para una imagen propia es el
                // formato en mayúsculas (`PNG`); la misma forma aquí.
                format: t
                    .mimetype
                    .rsplit('/')
                    .next()
                    .unwrap_or("image")
                    .to_ascii_uppercase(),
                width: t.width,
                height: t.height,
            },
            clamp_display(nombre),
        ));
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        Some(self.parche(vec![cambio]))
    }

    pub(super) fn aplicar_imagen(
        &mut self,
        token: RequestToken,
        leido: Result<Vec<u8>, Error>,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        if self.visor_token != Some(token) {
            return None;
        }
        let Ok(bytes) = leido else {
            return None;
        };
        if bytes.len() as u64 > IMAGEN_CAP {
            // No cabe. Se dice y se enseña la vista cruda: enseñarla a medias
            // sería enseñar otra imagen.
            self.status.message = Some(clamp_display(norte_i18n::t_in(
                self.lang,
                "viewer-image-too-large",
            )));
            let cambio = ViewChange::Status(self.status.clone());
            return Some(self.parche(vec![cambio]));
        }
        self.imagen = Some(std::sync::Arc::new(bytes));
        let cambio = ViewChange::Viewer {
            viewer: self.vista_visor(),
        };
        Some(self.parche(vec![cambio]))
    }
}
