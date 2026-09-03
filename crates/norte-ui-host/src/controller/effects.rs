//! Resolver un `Efecto` del catálogo compartido.
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
    /// Ejecuta lo que un comando pide sobre el hueco con el foco.
    ///
    /// Es el MISMO camino que toman las acciones directas del renderer (un
    /// click, un arrastre): que una tecla y un gesto que significan lo mismo
    /// hagan lo mismo no puede depender de que alguien se acuerde.
    ///
    /// **Es un DESPACHADOR, y por eso crece una línea por cada gesto nuevo.**
    /// Lo que el lint mide aquí no dice nada sobre su complejidad: cada brazo
    /// es un nombre y una llamada, y el `match` exhaustivo es justo lo que
    /// hace que añadir un `Efecto` sin atenderlo sea un error de compilación.
    /// Repartir los brazos por funciones para bajar del umbral esconde ese
    /// reparto en un segundo sitio sin mejorar nada — ya se hizo tres veces,
    /// y las tres volvió a rozarlo el gesto siguiente. Los grupos que SÍ
    /// significan algo —lo que abre, lo que dispone, lo que actúa sobre
    /// entradas— están agrupados; el resto se queda aquí a la vista.
    #[expect(
        clippy::too_many_lines,
        reason = "despachador exhaustivo: un brazo por gesto, sin lógica dentro"
    )]
    pub(super) fn aplicar_efecto(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // El foco puede estar en un panel que NO es un listado y que SÍ toma
        // teclas —hoy, el de procesos—. Entonces «bajar» es bajar por ÉL:
        // hasta ahora el rol `active` lo pintaba enfocado y las flechas movían
        // el listado de al lado, que es media función y la mitad que no se ve.
        //
        // Se decide por EFECTO y no por tecla, así que `j`, `↓` y `g g`
        // funcionan igual: el keymap dice qué comando es, y la superficie con
        // el foco dice qué significa ahí.
        // Cancelar se decide ANTES que nada: con el panel de procesos
        // enfocado y el tablero vacío, `efecto_en_panel_enfocado` responde
        // «aplicado» a cualquier efecto, y eso convertiría un «no hay nada
        // que parar» en un silencio.
        if matches!(efecto, Efecto::CancelarTask) {
            return self.cancelar_por_comando();
        }
        // Recorrer y descartar el tablero, por el mismo motivo y antes del
        // foco: son comandos del TABLERO, no del panel que lo pinta, y con el
        // panel de procesos cerrado tienen que seguir significando lo mismo.
        if let Efecto::TaskVecina { atras } = efecto {
            return self.mover_en_tablero(atras);
        }
        if matches!(efecto, Efecto::DescartarTask) {
            return self.descartar_task();
        }
        if let Some(salida) = self.efecto_en_panel_enfocado(efecto) {
            return salida;
        }
        if self.sitios_tienen_el_foco() && matches!(efecto, Efecto::Entrar | Efecto::Marcar) {
            // Entrar y plegar los atiende la barra lateral, y el `cd` que
            // salga va al LISTADO por el mismo camino que cualquier otro: es
            // lo que hace que tenerla abierta no cambie a dónde van las
            // operaciones.
            return self.activar_sitio_del_cursor(backend, buzon);
        }
        let slot = self.activo();
        match efecto {
            Efecto::Cursor(_)
            | Efecto::Pagina(_)
            | Efecto::Extremo { .. }
            | Efecto::Entrar
            | Efecto::Subir
            | Efecto::Rastro { .. }
            | Efecto::Marcar
            | Efecto::MarcarTodo
            | Efecto::InvertirMarcas
            | Efecto::MarcarExtension { .. }
            | Efecto::MarcarClase { .. }
            | Efecto::RestaurarMarcas
            | Efecto::MarcarSubiendo
            | Efecto::MarcarPagina { .. }
            | Efecto::MarcarHastaElBorde { .. }
            | Efecto::DesmarcarTodo => self.efecto_de_listado(efecto, slot, backend, buzon),
            Efecto::Foco { atras } => self.mover_foco(atras),
            Efecto::Destino => self.designar_destino(),
            // Atendido arriba, antes del panel enfocado. El brazo existe
            // porque el `match` es exhaustivo a propósito: un efecto nuevo
            // sin sitio tiene que ser un error de compilación.
            // Los tres del TABLERO se atienden antes de llegar aquí: no
            // dependen del panel que tenga el foco.
            Efecto::CancelarTask | Efecto::TaskVecina { .. } | Efecto::DescartarTask => {
                self.cancelar_por_comando()
            }
            Efecto::Tamano(_)
            | Efecto::Igualar
            | Efecto::Disposiciones
            | Efecto::Partir { .. }
            | Efecto::CerrarHueco
            | Efecto::AlternarHueco { .. }
            | Efecto::PestanaNueva
            | Efecto::CerrarPestana
            | Efecto::CiclarPestana { .. }
            | Efecto::MoverPestana { .. }
            | Efecto::IrAPestana { .. } => {
                self.efecto_de_disposicion(efecto, backend, buzon)
            }
            Efecto::Ordenar(col) => self.ordenar_por_columna(slot, col),
            Efecto::Refrescar => self.refrescar_visibles(backend, buzon),
            Efecto::AlternarOcultos => self.alternar_ocultos(),
            Efecto::CiclarEncoding => self.ciclar_encoding(),
            Efecto::Espejo | Efecto::EspejoObjetivo | Efecto::Traer | Efecto::Intercambiar => {
                self.gesto_de_panel(efecto, backend, buzon)
            }
            Efecto::VolumenesDeLado { derecha } => {
                self.abrir_volumenes_de_lado(derecha, backend, buzon)
            }
            Efecto::Columnas => self.abrir_columnas(),
            Efecto::Buscar => self.pedir_busqueda(),
            Efecto::BuscarRapido => self.buscar_rapido(),
            Efecto::CrearDirectorio
            | Efecto::CrearFichero
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa
            | Efecto::RenameLote
            // #314: cambiar permisos escribe, así que una ventana de solo
            // lectura tampoco lo hace.
            | Efecto::Permisos
            | Efecto::BuscarSemantica
            | Efecto::Sincronizar
            // Los dos que LANZAN un proceso: lo que ese proceso haga con los
            // ficheros no lo decide esta ventana.
            | Efecto::AbrirExterno
            | Efecto::Terminal
                if self.efectos == crate::commands::Efectos::SoloLectura =>
            {
                Self::no_muta()
            }
            // Copiar la ruta no toca nada y va en los dos modos: poner texto
            // en el portapapeles es tan de solo mirar como leer un nombre.
            Efecto::CopiarRuta => self.copiar_rutas(),
            Efecto::Sumas { verificar } => self.lanzar_sumas(verificar, backend, buzon),
            Efecto::MarcarPatron { marcar } => self.pedir_patron(marcar),
            Efecto::AbrirExterno => self.abrir_externo(),
            Efecto::Terminal => self.abrir_terminal(),
            Efecto::Comparar => self.pedir_comparacion(backend, buzon),
            Efecto::Desconectar => self.desconectar(backend, buzon),
            Efecto::TamanoDeDirectorio
            | Efecto::Empaquetar
            | Efecto::Desempaquetar
            | Efecto::ComprobarArchivo
            | Efecto::PartirFichero
            | Efecto::Juntar => self.efecto_sobre_entradas(efecto, backend, buzon),
            // Como comparar: necesita el backend porque sale a preguntar en
            // cuanto se abre, y el panel nace diciendo que planifica.
            Efecto::Sincronizar => self.pedir_sincronizacion(backend, buzon),
            Efecto::Paleta
            | Efecto::Ayuda
            | Efecto::Ajustes
            | Efecto::Extensiones
            | Efecto::Agentes
            | Efecto::Tema
            | Efecto::Menu
            | Efecto::PerfilElegir
            | Efecto::PerfilGuardarComo
            | Efecto::PerfilVecino { .. }
            | Efecto::Volumenes
            | Efecto::Conexiones
            // El historial y la hotlist son otros dos selectores: van con el
            // resto de lo que ABRE, y no cada uno con su brazo — este `match`
            // reparte, y crece un brazo por cada gesto nuevo.
            | Efecto::Historial
            | Efecto::Hotlist
            | Efecto::Ver => self.efecto_que_abre(efecto, backend, buzon),
            Efecto::CrearDirectorio
            | Efecto::CrearFichero
            | Efecto::Borrar { .. }
            | Efecto::Transferir { .. }
            | Efecto::Renombrar
            | Efecto::RenameIa
            | Efecto::RenameLote
            | Efecto::Permisos
            | Efecto::BuscarSemantica => self.efecto_que_muta(efecto),
        }
    }

    /// Los efectos que mueven el CURSOR o el listado: recorrer, entrar,
    /// subir, volver y marcar. Nada de esto escribe.
    pub(super) fn efecto_de_listado(
        &mut self,
        efecto: Efecto,
        slot: u32,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Cursor(delta) => self.aplicar(
                &UiAction::MoveCursor {
                    slot_id: slot,
                    delta,
                },
                backend,
                buzon,
            ),
            Efecto::Pagina(paginas) => {
                let filas = i64::from(self.hueco().visibles.max(1));
                self.aplicar(
                    &UiAction::MoveCursor {
                        slot_id: slot,
                        delta: paginas.saturating_mul(filas),
                    },
                    backend,
                    buzon,
                )
            }
            Efecto::Extremo { al_final } => {
                if al_final {
                    self.hueco_mut().pane.end();
                } else {
                    self.hueco_mut().pane.home();
                }
                (self.aplicada(), vec![self.parche_cursor()])
            }
            Efecto::Entrar => {
                // Una tecla actúa sobre lo que hay AHORA bajo el cursor, así
                // que la generación es la de este mismo instante.
                let key = RowKey(self.hueco().pane.cursor() as u64);
                let generation = self.hueco().pane.listing_epoch();
                self.navegacion(
                    &UiAction::Activate {
                        slot_id: slot,
                        key,
                        generation,
                    },
                    backend,
                    buzon,
                )
            }
            Efecto::Subir => self.navegacion(&UiAction::Parent { slot_id: slot }, backend, buzon),
            Efecto::Rastro { atras } => self.navegacion(
                &UiAction::History {
                    slot_id: slot,
                    back: atras,
                },
                backend,
                buzon,
            ),
            Efecto::Marcar => {
                let key = RowKey(self.hueco().pane.cursor() as u64);
                let generation = self.hueco().pane.listing_epoch();
                self.marcar(slot, key, generation)
            }
            Efecto::DesmarcarTodo => {
                self.hueco_mut().pane.clear_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarTodo => {
                self.hueco_mut().pane.mark_all();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::InvertirMarcas => {
                self.hueco_mut().pane.invert_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            // #313: la regla de qué es «la misma extensión», de qué cuenta
            // como fichero y de qué se restaura vive en `PaneState`, así que
            // aquí no se decide nada — es el mismo modelo que la terminal.
            Efecto::MarcarExtension { marcar } => {
                self.hueco_mut().pane.mark_same_extension(marcar);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarClase { dirs } => {
                self.hueco_mut().pane.mark_kind(dirs);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::RestaurarMarcas => {
                self.hueco_mut().pane.restore_previous_marks();
                (self.aplicada(), vec![self.parche_filas()])
            }
            // Marcar MOVIÉNDOSE: la regla entera —a qué avanza, qué decide si
            // el tramo se marca o se desmarca, y que los dos del borde limpien
            // el otro lado— vive en `PaneState`, igual que en la terminal. La
            // ventana repinta filas Y cursor porque estos SÍ lo mueven (menos
            // los del borde, que a propósito no).
            Efecto::MarcarSubiendo => {
                self.hueco_mut().pane.toggle_mark_and_retreat();
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarPagina { abajo } => {
                let n = self.hueco().pane.page_step();
                self.hueco_mut().pane.toggle_mark_page(n, abajo);
                (self.aplicada(), vec![self.parche_filas()])
            }
            Efecto::MarcarHastaElBorde { arriba } => {
                if arriba {
                    self.hueco_mut().pane.mark_to_top();
                } else {
                    self.hueco_mut().pane.mark_to_bottom();
                }
                (self.aplicada(), vec![self.parche_filas()])
            }
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
    }

    /// Manda un efecto NATIVO al proceso que hospeda, si hay alguien.
    ///
    /// `false` = nadie escucha. No es un error del host: un frontend que no
    /// sabe hacer estas cosas no se suscribe, y entonces lo honesto es
    /// decirle a quien pulsó que aquí eso no pasa, en vez de acusar recibo de
    /// algo que no va a ocurrir.
    pub(super) fn nativo(&self, efecto: crate::dto::NativeEffect) -> bool {
        self.escritorio
            .nativos
            .as_ref()
            .is_some_and(|tx| tx.send(efecto).is_ok())
    }

    /// Las rutas de lo MARCADO —o de lo señalado, si no hay marcas— al
    /// portapapeles.
    ///
    /// Marcado primero y cursor como respaldo: es la misma regla que copiar y
    /// mover, y tener dos respuestas a «sobre qué actúa esto» según el
    /// comando es lo que hace que un gesto se aplique a otra cosa.
    ///
    /// En BYTES y en forma nativa cuando la hay: lo que se pega tiene que
    /// abrir el mismo fichero, y una ruta decodificada con pérdida abre otro.
    pub(super) fn copiar_rutas(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let hueco = self.hueco();
        // `marked_paths` ya cae al cursor cuando no hay marcas: es la misma
        // regla que copiar y mover, y tener dos respuestas a «sobre qué
        // actúa esto» según el comando es lo que aplica un gesto a otra cosa.
        let paths: Vec<VPath> = hueco.pane.marked_paths();
        if paths.is_empty() {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        }
        let count = paths.len();
        let bytes = norte_frontend::shell::clipboard_bytes(&paths);
        if !self.nativo(crate::dto::NativeEffect::CopyBytes { bytes, count }) {
            return Self::sin_escritorio();
        }
        let fuera = self.decir_con("msg-paths-copied", &[("n", &count.to_string())]);
        (self.aplicada(), fuera)
    }

    /// Abre lo señalado con la aplicación que el escritorio elija.
    pub(super) fn abrir_externo(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let Some(path) = self.hueco().pane.selected().map(|e| e.path.clone()) else {
            return (
                ActionAck::Unavailable {
                    reason_key: "host-nothing-selected".to_owned(),
                },
                Vec::new(),
            );
        };
        // Solo lo que está en ESTE disco: a `xdg-open` no se le puede dar un
        // `sftp://`, y fingir que sí abriría otra cosa —o nada— sin decirlo.
        if !norte_frontend::shell::is_local(&path) {
            let fuera = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                fuera,
            );
        }
        if !self.nativo(crate::dto::NativeEffect::OpenPath { path }) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-external"))
    }

    /// Abre un terminal sentado en el directorio del panel activo.
    pub(super) fn abrir_terminal(&mut self) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        let dir = self.hueco().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            // Un terminal se sienta en un directorio del sistema de ficheros:
            // en un `sftp://` no hay dónde sentarlo, y abrirlo en el `$HOME`
            // sin decir nada sería abrirlo en otro sitio.
            let fuera = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                fuera,
            );
        }
        if !self.nativo(crate::dto::NativeEffect::OpenTerminal { dir }) {
            return Self::sin_escritorio();
        }
        (self.aplicada(), self.decir("msg-opening-terminal"))
    }

    /// Nadie escucha los efectos nativos: se DICE.
    pub(super) fn sin_escritorio() -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        (
            ActionAck::Unavailable {
                reason_key: "host-no-desktop".to_owned(),
            },
            Vec::new(),
        )
    }

    /// Los efectos que abren una PANTALLA sobre el listado y no tocan nada.
    pub(super) fn efecto_que_abre(
        &mut self,
        efecto: Efecto,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::Paleta => self.abrir_paleta(backend, buzon),
            Efecto::Ayuda => self.abrir_ayuda(backend, buzon),
            Efecto::Ajustes => self.abrir_ajustes(),
            Efecto::Extensiones => self.abrir_extensiones(backend, buzon),
            Efecto::Agentes => self.abrir_agentes(),
            Efecto::Tema => self.abrir_tema(),
            Efecto::Menu => self.abrir_menu(),
            Efecto::PerfilElegir => self.pedir_perfiles(None, buzon),
            Efecto::PerfilGuardarComo => self.pedir_guardar_perfil(),
            Efecto::PerfilVecino { atras } => self.pedir_perfiles(Some(!atras), buzon),
            Efecto::Volumenes => self.abrir_volumenes(backend, buzon),
            Efecto::Conexiones => self.abrir_conexiones(backend, buzon),
            Efecto::Historial => self.abrir_historial(),
            Efecto::Hotlist => self.abrir_hotlist(),
            Efecto::Ver => self.pedir_visor(backend, buzon),
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
    }

    /// Los efectos que ESCRIBEN. Ninguno muta aquí: los cinco abren la
    /// pregunta por la que pasa la mutación, que es la única puerta.
    pub(super) fn efecto_que_muta(
        &mut self,
        efecto: Efecto,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        match efecto {
            Efecto::CrearDirectorio => self.pedir_mkdir(),
            Efecto::CrearFichero => self.pedir_fichero_nuevo(),
            Efecto::Borrar { permanente } => self.pedir_borrado(permanente),
            Efecto::Transferir { mover } => self.pedir_transferencia(mover),
            Efecto::Renombrar => self.pedir_rename(),
            Efecto::RenameIa => self.pedir_instruccion_ia(),
            Efecto::RenameLote => self.pedir_plantilla_de_lote(None),
            Efecto::Permisos => self.pedir_permisos(),
            Efecto::BuscarSemantica => self.pedir_consulta_semantica(),
            // Los demás no llegan aquí: el `match` de arriba los reparte.
            _ => Self::no_muta(),
        }
    }
}
