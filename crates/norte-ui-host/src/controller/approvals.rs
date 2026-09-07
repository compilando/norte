//! Las aprobaciones de policy que llegan del daemon.
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
    /// Pide el catálogo de atributos de esta localización, si hace falta.
    ///
    /// Solo si hay columnas `attr:` configuradas y aún no se tiene el de su
    /// esquema: preguntar por un catálogo que nadie va a leer es un viaje de
    /// más en cada `cd`.
    pub(super) fn pedir_catalogo(
        &self,
        dir: &VPath,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) {
        if self.attrs_de(dir).is_empty() || self.catalogos.contains_key(dir.scheme()) {
            return;
        }
        let backend = Arc::clone(backend);
        let buzon = buzon.clone();
        let dir = dir.clone();
        let scheme = dir.scheme().to_owned();
        tokio::spawn(async move {
            // Un catálogo que no llega no rompe nada: las celdas se pintan
            // opacas, que es exactamente lo que se sabe de ellas.
            if let Ok(catalogo) = backend.attr_catalog(dir).await {
                let _ = buzon
                    .send(Mensaje::Catalogo(Box::new((scheme, catalogo))))
                    .await;
            }
        });
    }

    /// Qué se está pidiendo, en una línea (#314).
    ///
    /// Para todas las ops menos una es el nombre de la op: aprobar «copiar
    /// estas doce» ES la decisión. Un `set-mode` no, porque dos con las mismas
    /// rutas y modos distintos significan cosas opuestas, así que el modo va
    /// AQUÍ, con el sujeto — entre líneas de rutas, una ruta puede suplantar
    /// cualquier otra línea, y esta es la mitad de la decisión.
    pub(super) fn sujeto_de_aprobacion(
        &self,
        req: &norte_proto::methods::PolicyApprovalRequired,
    ) -> String {
        let base = match req.detail.mode {
            Some(mode) => norte_i18n::ta_in(
                self.lang,
                "modal-approval-op-mode",
                &[
                    ("op", &req.op),
                    ("mode", &norte_frontend::chmod::format_mode(mode)),
                ],
            ),
            None => req.op.clone(),
        };
        // #315: y el ALCANCE. Un recursivo sobre una raíz llega con
        // `paths_total = 1`, así que sin esto la pregunta decía «set-mode
        // sobre 1 ruta» y lo aprobado era el árbol entero.
        if !req.detail.recursive {
            return base;
        }
        let cola = match req.detail.dir_mode {
            Some(dir) => norte_i18n::ta_in(
                self.lang,
                "modal-approval-recursive-dirs",
                &[("mode", &norte_frontend::chmod::format_mode(dir))],
            ),
            None => norte_i18n::t_in(self.lang, "modal-approval-recursive"),
        };
        format!("{base} {cola}")
    }

    /// Abre el diálogo de una op de agente que espera decisión.
    ///
    /// Las rutas vienen REDACTADAS del servidor y son solo display: jamás se
    /// reparsean a una operación —la op real va ligada al `approval_id`—, y
    /// se pintan con el saneado canónico porque las controla quien pidió la
    /// operación.
    /// Las dos respuestas de una aprobación de agente.
    ///
    /// Fuera del constructor porque el constructor ya no cabía, y aparte
    /// porque estas dos etiquetas no son las de un diálogo normal: `approve`
    /// y `deny` se llaman distinto de `confirm`/`cancel` a propósito — en una
    /// superficie de seguridad, «confirmar» y «aprobar» no deberían poder
    /// confundirse en un renderer.
    fn aprobar_o_denegar() -> Vec<DialogChoice> {
        vec![
            DialogChoice {
                id: "approve".to_owned(),
                label_key: "dialog-approve".to_owned(),
                // Aprobar una mutación de un agente ES destructivo: el
                // renderer la pinta como tal, y Enter no la dispara sola
                // porque no hay respuesta por defecto.
                destructive: true,
            },
            DialogChoice {
                id: "deny".to_owned(),
                label_key: "dialog-deny".to_owned(),
                destructive: false,
            },
        ]
    }

    pub(super) fn abrir_aprobacion(
        &mut self,
        req: &norte_proto::methods::PolicyApprovalRequired,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        // La MISMA aprobación puede llegar dos veces: el SDK resincroniza
        // `policy.pending` en cada reconexión, y lo que sigue vivo vuelve por
        // el canal. Dos diálogos son dos respuestas, y la segunda cae sobre
        // un id que el daemon ya cerró.
        if self.dialogos.iter().any(|d| {
            matches!(
                d.al_confirmar,
                Some(Pendiente::Decidir { approval_id, .. }) if approval_id == req.approval_id
            )
        }) {
            return Vec::new();
        }
        // Se APUNTA la sesión que pidió, aunque el diálogo no llegue a
        // abrirse por lo que sea: es lo ÚNICO que nombra a un agente en todo
        // el protocolo, y sin ese apunte no hay forma de ofrecer deshacer lo
        // que hizo salvo tecleando su id a mano (#276).
        let mut fuera = Vec::new();
        if let Some(sesion) = req.session.as_deref() {
            self.agencia.sesiones.vista(sesion, &req.op);
            // Y se REPINTA si el panel está abierto. La lista cambia SIN
            // gesto —esta petición la reordena— y un renderer al que no se
            // le dice se queda pintando el orden de antes: la fila que el
            // lector ve resaltada deja de ser la que el host tiene elegida, y
            // `u` deshace el trabajo de otra sesión.
            if self.agencia.panel {
                fuera.push(self.parche(vec![ViewChange::Agents {
                    agents: self.vista_agentes(),
                }]));
            }
        }
        // Estas rutas vienen del daemon como TEXTO ya redactado, no como
        // `VPath`, así que el enmascarado es el de cadenas y la marca se
        // calcula comparando: si enmascarar cambió algo, lo que se lee no es
        // lo que hay, y quien aprueba tiene que verlo.
        let linea = |texto: &str| {
            let enmascarado = norte_encoding::mask_terminal_hazards(texto);
            // DOS motivos para marcar, y el segundo es el que faltaba: estas
            // rutas llegan REDACTADAS del daemon, que ya pasó los bytes por
            // `display_lossy` —controles, overrides bidi y bytes inválidos ya
            // son U+FFFD—, así que comparar contra el original no detecta
            // nada de eso y la marca no saltaba justo en la clase más
            // peligrosa. Encima era inconsistente: un `zwsp` sí la encendía,
            // porque el lossy del daemon no lo toca.
            //
            // El carácter de sustitución ES la señal de que lo que se lee no
            // es lo que hay. No se puede recuperar qué había —por eso el
            // daemon manda texto y no `VPath`— pero sí decir que no es fiel.
            let hostil = enmascarado != texto || texto.contains('\u{FFFD}');
            crate::dto::DialogLine {
                text: clamp_display(enmascarado),
                hostile: hostil,
            }
        };
        // El cuerpo son SOLO las rutas: el renderer las numera por posición,
        // que es una etiqueta que ningún nombre de fichero puede escribir. Lo
        // demás —qué se pide, quién lo pide, cuándo caduca— va en campos
        // propios, por el mismo motivo que el destino de una transferencia:
        // entre líneas de rutas, una ruta suplanta a cualquier otra línea.
        let cuerpo: Vec<crate::dto::DialogLine> = req
            .paths
            .iter()
            .take(Self::MAX_LINEAS_DIALOGO)
            .map(|p| linea(p))
            .collect();
        let sujeto = linea(&self.sujeto_de_aprobacion(req));
        // Quién pide es lo PRIMERO que hace falta para decidir, y se
        // descartaba: el título dice «aprobación de agente» y sin esto no se
        // sabe de qué agente.
        let quien = req.session.as_deref().map(linea);
        // Si la lista viene RECORTADA hay que decirlo: aprobar creyendo que
        // son tres rutas cuando son mil es aprobar otra cosa (0.36.0). Y son
        // DOS recortes: el del daemon (`paths_total`) y el nuestro. El
        // recuento honesto es el mayor de los dos.
        //
        // La frase va en `overflow_note` y no como una línea más del cuerpo,
        // por el mismo motivo que el destino de una transferencia tiene campo
        // propio: entre líneas de rutas, una ruta la puede suplantar. Antes
        // era una línea Y encima citaba `modal-approval-truncated`, una clave
        // Fluent que no existe en ningún idioma — o sea que un lote recortado
        // pintaba el identificador crudo.
        let total = std::cmp::max(req.paths_total, req.paths.len() as u64);
        let mostrados = req.paths.len().min(Self::MAX_LINEAS_DIALOGO);
        let nota = self.nota_de_recorte(mostrados, usize::try_from(total).unwrap_or(usize::MAX));
        // Cuánto le queda, DICHO y en su propio campo. Una decisión con fecha
        // de caducidad que no la enseña se lee como una que espera para
        // siempre, y quien vuelve al rato pulsa aprobar sobre algo que el
        // daemon ya denegó.
        //
        // Con `ttl_ms == 0` —DESCONOCIDO: una pendiente reconstruida por el
        // resync de `policy.pending` no transporta el TTL restante— se dice
        // que no se sabe, en vez de callar: callar deja el diálogo delante
        // invitando a aprobar sobre un id que el daemon puede haber reapado
        // hace rato. Y sin línea de plazo, un fichero llamado «caduca en
        // 3600 s» sería la única que lo pareciera.
        let plazo = Some(if req.ttl_ms > 0 {
            clamp_display(norte_i18n::ta_in(
                self.lang,
                "modal-approval-ttl",
                &[("s", &req.ttl_ms.div_ceil(1000).to_string())],
            ))
        } else {
            clamp_display(norte_i18n::t_in(self.lang, "modal-approval-ttl-unknown"))
        });
        // Y CUÁNDO vence, para que el renderer cuente en vez de repetir una
        // frase congelada (#279). Solo con un TTL conocido: contar hacia atrás
        // desde un plazo inventado sería peor que no contar.
        let vence_en = (req.ttl_ms > 0)
            .then(|| i64::try_from(req.ttl_ms).ok().map(|ms| ahora_ms() + ms))
            .flatten();
        let id = ModalId(self.siguiente_modal);
        self.siguiente_modal += 1;
        let vista = DialogView {
            id,
            title_key: "modal-approval-title".to_owned(),
            destination: None,
            subject: Some(sujeto),
            asker: quien,
            deadline: plazo,
            deadline_at_ms: vence_en,
            body: cuerpo,
            overflow_note: nota,
            choices: Self::aprobar_o_denegar(),
            input: None,
            input_hostile: false,
            input_secret: false,
            dest_check: crate::dto::DestCheckView::NotAsked,
        };
        let caidos = self.apilar_dialogo(Dialogo {
            id,
            vista: vista.clone(),
            tecleado: Tecleado::Texto(String::new()),
            // Se abre SOLA: la trae una op de un agente, no una tecla.
            reconocido: false,
            al_confirmar: Some(Pendiente::Decidir {
                approval_id: req.approval_id,
                session: req.session.clone(),
            }),
        });
        // Y se programa su caducidad. El daemon deja de aceptar el id cuando
        // el TTL se acaba: un diálogo que siguiera delante invitaría a
        // aprobar en el vacío, y quien lo hiciera se quedaría creyendo que
        // autorizó lo que en realidad quedó denegado por silencio.
        if req.ttl_ms > 0 {
            let buzon = buzon.clone();
            let approval_id = req.approval_id;
            let plazo = std::time::Duration::from_millis(req.ttl_ms);
            tokio::spawn(async move {
                tokio::time::sleep(plazo).await;
                let _ = buzon.send(Mensaje::AprobacionCaducada(approval_id)).await;
            });
        }
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        let mut salidas = vec![self.parche(vec![cambio])];
        salidas.extend(caidos);
        salidas
    }

    /// El TTL de una aprobación se acabó: su diálogo se cierra y se dice.
    ///
    /// No se manda `policy.decide`: el daemon ya la resolvió por su cuenta
    /// —un TTL vencido es una denegación—, y contestar sobre un id cerrado
    /// solo produce un error que no significa nada para quien lo lee.
    pub(super) fn caduca_aprobacion(&mut self, approval_id: u64) -> Vec<BridgeEnvelope<UiUpdate>> {
        let antes = self.dialogos.len();
        self.dialogos.retain(|d| {
            !matches!(
                d.al_confirmar,
                Some(Pendiente::Decidir { approval_id: id, .. }) if id == approval_id
            )
        });
        if self.dialogos.len() == antes {
            // Ya se había contestado: la caducidad llega y no hay nada que
            // cerrar. No es un error, y no se dice nada.
            return Vec::new();
        }
        let cambio = ViewChange::Dialogs {
            dialogs: self.vistas_de_dialogos(),
        };
        let mut salidas = vec![self.parche(vec![cambio])];
        // NOMBRA la que caducó (#279). Con dos apiladas, «la aprobación
        // caducó» no dice cuál se cerró sola ni cuál sigue esperando.
        salidas.extend(self.decir_con("msg-approval-expired", &[("id", &approval_id.to_string())]));
        salidas
    }
}
