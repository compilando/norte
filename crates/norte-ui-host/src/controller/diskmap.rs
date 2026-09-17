//! El mapa de disco, en la ventana (fase 4).
//!
//! El estado del mapa es el COMPARTIDO (`norte_frontend::diskmap`), el mismo
//! que usa el terminal: qué directorio se describe, lo medido y cuál es el hijo
//! elegido. Y el reparto en rectángulos es el compartido también
//! (`norte_frontend::treemap::squarify`). Lo de aquí es el CABLEADO: pedir la
//! medida, aterrizarla y resolver un clic.
//!
//! El molde es el del panel de plugin, a propósito: un estado por hueco, una
//! petición viva con su testigo, y una respuesta que llega con otro testigo se
//! tira. Lo que cambia es qué se pide —una medida, no un marco— y que aquí lo
//! que se conserva entre repintados es lo MEDIDO, que cuesta minutos.
//!
//! # Por qué reparte el HOST y no el renderer
//! Un treemap calculado dos veces son dos treemaps distintos en cuanto alguien
//! toque un redondeo, y entonces el rectángulo que se pinta y el que resuelve
//! un clic dejan de ser el mismo — o sea, pulsas uno y se abre el de al lado.
//! Misma regla que el panel de plugin (ADR 0077), y aquí con más motivo: lo que
//! hay al otro lado de un clic es un fichero.
//!
//! # El mapa NO sigue al cursor
//! Su firma es el DIRECTORIO, no la fila. Por eso está en `NO_SIGUEN` y por eso
//! mover el cursor no vuelve a medir: sondear por cursor convertiría bajar por
//! un `$HOME` en una tormenta de medidas de minutos.

use std::sync::Arc;

use norte_frontend::layout::SlotId;
use norte_proto::VPath;
use tokio::sync::mpsc;

use super::{Estado, Mensaje, RequestToken, kind_de};
use crate::backend::HostBackend;
use crate::bridge::{BridgeEnvelope, clamp_display};
use crate::dto::UiUpdate;

/// El kind que ocupa un hueco de mapa de disco.
pub(super) const KIND: &str = "disk-map";

/// Lo que un hueco de mapa tiene AHORA y lo que está pidiendo.
#[derive(Default)]
pub(super) struct EstadoMapa {
    /// Lo medido, con su directorio y su elección. El estado COMPARTIDO.
    pub(super) mapa: norte_frontend::diskmap::DiskMap,
    /// El directorio de la última medida pedida —o intentada y fallida—.
    ///
    /// Las dos cosas en un campo porque contestan la misma pregunta: ¿hace
    /// falta pedir esto? Sin anotar el intento fallido, un directorio que no se
    /// deja medir se repide tras cada mensaje del actor.
    pub(super) pedido: Option<VPath>,
    /// La petición en vuelo, con su testigo.
    pub(super) en_vuelo: Option<(RequestToken, VPath)>,
}

impl Estado {
    /// Qué directorio debería estar describiendo el mapa del hueco `slot`.
    ///
    /// El vínculo se resuelve con el motor compartido, igual que el preview, la
    /// hoja y el panel de plugin: un hueco seguido que muere degrada al rol
    /// `active`. Devuelve también el HUECO, porque el clic navega ESE listado y
    /// no el del mapa.
    fn seguido_de_mapa(&self, slot: SlotId) -> Option<(u32, VPath)> {
        let mut diags = Vec::new();
        let seguido =
            norte_frontend::layout::resolve_follow(&self.arbol, slot, &self.roles, &mut diags)
                .or_else(|| self.roles.get(norte_frontend::layout::RoleId::Active))
                .unwrap_or(SlotId(self.activo()));
        let SlotId(id) = seguido;
        let hueco = self.huecos.get(&id)?;
        Some((id, hueco.pane.dir().clone()))
    }

    /// Pide la medida de los mapas colocados cuyo directorio cambió.
    ///
    /// Se llama tras CADA mensaje del actor, como sus vecinos, así que lo
    /// primero es salir barato cuando no hay nada que hacer: recorrer el árbol
    /// para descubrir que no hay ningún mapa se paga en cada tecla de cada
    /// sesión que no lo usa.
    pub(super) fn sondear_mapas(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        let huecos: Vec<SlotId> = self
            .reparto
            .placements
            .iter()
            .filter(|(slot, _)| kind_de(&self.arbol, *slot).is_some_and(|k| k.as_str() == KIND))
            .map(|(slot, _)| *slot)
            .collect();
        if huecos.is_empty() && self.mapas.is_empty() {
            return Vec::new();
        }
        // Un hueco que ya no existe no guarda nada: un `SlotId` se reutiliza, y
        // sin podar, el mapa de un hueco nuevo heredaría lo medido del anterior
        // — los tamaños de otro directorio, bajo este título.
        let vivos: Vec<u32> = huecos.iter().map(|SlotId(id)| *id).collect();
        self.mapas.retain(|id, _| vivos.contains(id));

        for slot in huecos {
            let SlotId(id) = slot;
            let Some((_, dir)) = self.seguido_de_mapa(slot) else {
                continue;
            };
            let est = self.mapas.entry(id).or_default();
            if est.pedido.as_ref() == Some(&dir) || est.en_vuelo.is_some() {
                continue;
            }
            // Apuntar OLVIDA lo medido: el mapa del directorio anterior bajo el
            // título del nuevo es la respuesta equivocada durante justo el rato
            // que dura la medida, que es cuando alguien lo mira.
            if est.mapa.dir() != Some(&dir) {
                est.mapa.apuntar(dir.clone());
            }
            self.token += 1;
            let token = RequestToken(self.token);
            self.mapas.entry(id).or_default().en_vuelo = Some((token, dir.clone()));

            let params = norte_proto::methods::FsDirUsageParams {
                path: dir.clone(),
                // Un nivel: es lo que pinta un mapa, y es lo único que el
                // servidor sirve hoy. Pedir más se RECHAZA (ADR 0117).
                depth: 1,
            };
            let backend = Arc::clone(backend);
            let buzon = buzon.clone();
            tokio::spawn(async move {
                // El plazo gobierna el LANZAMIENTO, no la medida: `fs.dir_usage`
                // devuelve la Task en cuanto la encola, y medir un `$HOME`
                // puede tardar minutos. Un plazo sobre la medida la mataría
                // justo en los árboles para los que existe.
                let lanzada =
                    match tokio::time::timeout(super::PLAZO_PLUGINS, backend.dir_usage(params))
                        .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(norte_proto::Error::ProviderUnavailable { retryable: true }),
                    };
                let task = match lanzada {
                    Ok(t) => t,
                    Err(e) => {
                        let _ = buzon
                            .send(Mensaje::MapaContenido(Box::new((id, token, Err(e)))))
                            .await;
                        return;
                    }
                };
                let task_id = task.id;
                let mut prog = task.progress;
                // El informe solo es DEFINITIVO cuando la Task es terminal.
                // Pedirlo antes daría medio mapa sin decir que lo es, y medio
                // mapa se lee como un directorio pequeño.
                while !prog.borrow().state.is_terminal() {
                    if prog.changed().await.is_err() {
                        break;
                    }
                }
                let estado = prog.borrow().state.clone();
                let res = if estado.is_terminal() {
                    backend
                        .dir_usage_report(task_id)
                        .await
                        .map(|informe| (estado, informe))
                } else {
                    // El canal murió sin llegar a terminal: el daemon se cayó.
                    Err(norte_proto::Error::ProviderUnavailable { retryable: true })
                };
                let _ = buzon
                    .send(Mensaje::MapaContenido(Box::new((id, token, res))))
                    .await;
            });
        }
        Vec::new()
    }

    /// Aterriza una medida: se enseña si el testigo es el de la última petición
    /// de ESE hueco, y se tira si no.
    ///
    /// **Y se comprueba el DIRECTORIO además del testigo.** Medir tarda, y en
    /// ese rato el panel puede estar apuntando a otro sitio: un informe
    /// aterrizado sin mirarlo pintaría los tamaños de un directorio bajo el
    /// título de otro.
    pub(super) fn aterrizar_mapa(
        &mut self,
        slot: u32,
        token: RequestToken,
        res: Result<
            (
                norte_proto::TaskState,
                norte_proto::methods::FsDirUsageReportResult,
            ),
            norte_proto::Error,
        >,
    ) -> Option<BridgeEnvelope<UiUpdate>> {
        let est = self.mapas.get_mut(&slot)?;
        if est.en_vuelo.as_ref().map(|(t, _)| *t) != Some(token) {
            return None;
        }
        let (_, dir) = est.en_vuelo.take()?;
        // El intento queda anotado pase lo que pase: sin esto, un directorio
        // que no se deja medir se repide tras cada mensaje del actor.
        est.pedido = Some(dir.clone());
        if est.mapa.dir() != Some(&dir) {
            return None; // llegó tarde: el panel ya está en otro sitio
        }
        let (estado, informe) = match res {
            Ok(par) => par,
            Err(e) => {
                // El motivo acaba en el TÍTULO del panel, así que va traducido
                // al idioma de la sesión y acotado, como el de la búsqueda.
                est.mapa
                    .fallo(clamp_display(norte_frontend::error::error_category_in(
                        self.lang, &e,
                    )));
                let snap = self.snapshot();
                return Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))));
            }
        };
        let completa = estado == norte_proto::TaskState::Completed;
        est.mapa.aterrizar(informe, completa);
        let snap = self.snapshot();
        Some(self.sobre(UiUpdate::Snapshot(Box::new(snap))))
    }

    /// Un clic sobre un rectángulo: entra en ese hijo.
    ///
    /// Se resuelve contra el MISMO reparto que se pintó —`vista_de_mapa` usa el
    /// tamaño de dentro del borde y esto también—, así que el rectángulo que se
    /// ve y el que responde son el mismo por construcción.
    ///
    /// **Sin `zona_puede`**: ese filtro existe porque en un panel de plugin la
    /// etiqueta y el comando los elige un tercero y nada los ata. Aquí los pone
    /// `squarify`, así que filtrarlos sería protegerse de uno mismo.
    pub(super) fn clic_en_mapa(
        &mut self,
        slot: u32,
        row: u16,
        col: u16,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (crate::bridge::ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        // Un hueco ESCONDIDO conserva su mapa, así que sus zonas seguirían
        // resolviéndose aunque nadie las vea. El renderer no pinta lo
        // escondido, luego un clic ahí no viene de una persona.
        if self.oculto(slot) {
            return (Self::obsoleta(crate::StaleAction::Generation), Vec::new());
        }
        let Some((cols, rows)) = self
            .reparto
            .placements
            .iter()
            .find(|(SlotId(s), _)| *s == slot)
            .map(|(_, r)| (r.width.saturating_sub(2), r.height.saturating_sub(2)))
        else {
            return (self.aplicada(), Vec::new());
        };
        let elegido = self.mapas.get(&slot).and_then(|e| {
            let marco = norte_frontend::treemap::squarify(&e.mapa.informe().children, cols, rows);
            let arg = marco.hit_at(row, col)?.arg.clone()?;
            let seg = norte_proto::Segment::parse_wire(&arg).ok()?;
            // Solo un DIRECTORIO se abre: el mapa enseña las dos cosas, y
            // «entrar» en un fichero no es navegar.
            let hijo = e.mapa.informe().children.iter().find(|c| c.name == seg)?;
            (hijo.kind == norte_proto::EntryKind::Dir).then_some(seg)
        });
        let Some(seg) = elegido else {
            // Una celda sin rectángulo, o un fichero: no pasa nada, y no es un
            // error del lector.
            return (self.aplicada(), Vec::new());
        };
        // Señalar TAMBIÉN elige: el teclado y el ratón dejan el mapa en el
        // mismo sitio, que es lo que hace que pulsar y luego usar las flechas
        // siga desde donde estabas.
        if let Some(e) = self.mapas.get_mut(&slot) {
            e.mapa.elegir(&seg);
        }
        let Some((destino_slot, dir)) = self.seguido_de_mapa(SlotId(slot)) else {
            return (self.aplicada(), Vec::new());
        };
        let destino = dir.join(seg);
        // Navegar el LISTADO seguido, no el mapa: el mapa señala, y el `cd` va
        // por donde va cualquier otro (ADR 0077). `Record` porque esto es un
        // movimiento que pidió el lector: entra en el rastro y poda el forward.
        let updates = self.navegar_hueco(
            destino_slot,
            &destino,
            norte_frontend::nav::Trail::Record,
            backend,
            buzon,
        );
        (self.aplicada(), updates)
    }

    /// Proyecta el mapa de disco de un hueco a lo que el renderer pinta.
    ///
    /// El marco se reparte con el tamaño de DENTRO del borde, igual que la
    /// firma de un panel de plugin: quien describe el contenido no sabe dónde
    /// cayó su hueco, así que la cuenta la hace quien pinta — y aquí el host
    /// pinta y resuelve, de modo que las dos cuentas son la misma.
    ///
    /// Sin hueco colocado no hay tamaño, y entonces no hay mapa: se manda vacío
    /// con su título, como un panel cuyo primer marco no ha llegado.
    pub(super) fn vista_de_mapa(&self, id: u32) -> crate::dto::DiskMapSlotView {
        let est = self.mapas.get(&id);
        // El título es el NOMBRE del directorio que se describe, no su ruta: el
        // hueco es estrecho y la ruta entera no cabe. Sale de un nombre de
        // fichero, así que se enmascara como cualquier otro.
        let (title, title_hostile) = est.and_then(|e| e.mapa.dir()).map_or_else(
            || (String::new(), false),
            |d| {
                d.file_name().map_or_else(
                    // La raíz de un provider no tiene nombre base: se dice con
                    // su esquema en vez de dejar el título en blanco.
                    || (d.scheme().to_owned(), false),
                    |n| norte_frontend::display_name(n.as_bytes()),
                )
            },
        );

        let celdas = self
            .reparto
            .placements
            .iter()
            .find(|(SlotId(s), _)| *s == id)
            .map(|(_, r)| (r.width.saturating_sub(2), r.height.saturating_sub(2)));

        let (lines, hits) = match (est, celdas) {
            (Some(e), Some((cols, rows))) => {
                let marco =
                    norte_frontend::treemap::squarify(&e.mapa.informe().children, cols, rows);
                let lines = marco
                    .lines
                    .iter()
                    .map(|linea| linea.iter().map(super::views::span_view).collect())
                    .collect();
                let hits = marco
                    .hits
                    .iter()
                    .map(|h| crate::dto::HitView {
                        row: h.row,
                        col: h.col,
                        width: h.width,
                    })
                    .collect();
                (lines, hits)
            }
            _ => (Vec::new(), Vec::new()),
        };

        crate::dto::DiskMapSlotView {
            slot_id: id,
            title: clamp_display(title),
            title_hostile,
            lines,
            hits,
            measuring: est.is_some_and(|e| e.en_vuelo.is_some()),
        }
    }
}
