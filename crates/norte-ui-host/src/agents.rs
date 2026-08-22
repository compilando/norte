//! Las sesiones de AGENTE que esta ventana ha visto, y el deshacer de una
//! entera (#276).
//!
//! **De dónde sale la lista, y por qué eso importa.** No hay método en el
//! protocolo que enumere las sesiones de agente vivas: lo único que las
//! nombra es la petición de aprobación que un agente dispara
//! (`policy.approval_required`, cuyo `session` es opcional). Así que esta
//! lista es exactamente «las que ESTA ventana ha visto pedir permiso», y la
//! propia pantalla lo dice — una lista que se presenta como el censo de
//! agentes del sistema y no lo es sería peor que no tenerla.
//!
//! Lo que compra frente a teclear el id a mano, que es lo que la tarea 5.3
//! rechazó: el operando se ELIGE. Un id de sesión tecleado por un humano en
//! una superficie de gobierno es un id que se puede equivocar, y deshacer la
//! sesión equivocada es deshacer el trabajo de otro.
//!
//! El id de una sesión es una clave OPACA del daemon: se pinta enmascarada
//! —puede llevar cualquier byte— y viaja CRUDA, porque es la clave con la que
//! el core la resuelve.

use crate::bridge::clamp_display;
use crate::dto::{AgentRowView, AgentsView};

/// Tope de sesiones recordadas.
///
/// Un daemon que anuncia peticiones sin parar no puede hacer crecer esto sin
/// fin. Se olvida la MÁS VIEJA por última vez vista, que es la que menos
/// probablemente esté todavía haciendo algo.
const MAX_SESIONES: usize = 128;

/// Lo que se sabe de una sesión de agente.
#[derive(Debug, Clone)]
struct Sesion {
    /// El id CRUDO, tal como llegó: es lo que vuelve al daemon.
    id: String,
    /// Cuántas peticiones suyas ha visto esta ventana.
    vistas: u32,
    /// Cuántas se le aprobaron DESDE AQUÍ.
    aprobadas: u32,
    /// El último op-kind que pidió, ya enmascarado y con su bandera.
    ultima: (String, bool),
    /// El orden en que se la vio por última vez: manda el más alto.
    sello: u64,
}

/// Las sesiones vistas, y el panel abierto sobre ellas.
#[derive(Debug, Default)]
pub(crate) struct Agentes {
    /// Lo visto, por id.
    sesiones: std::collections::HashMap<String, Sesion>,
    /// El reloj lógico de «visto por última vez».
    reloj: u64,
    /// Cuál está elegida, POR ID y no por posición.
    ///
    /// La lista se reordena sola —una petición nueva sube a su sesión al
    /// primer puesto— y una selección por índice significa otra fila en
    /// cuanto eso pasa. Es la misma regla que la 6.2 dejó escrita para las
    /// filas comparadas: se nombran por id, jamás por posición.
    elegida: Option<String>,
    /// Cuántas veces ha CAMBIADO la lista.
    ///
    /// Viaja con la vista y vuelve con el clic: un clic se resuelve contra la
    /// lista que el lector estaba mirando, no contra la de ahora. Sin esto,
    /// una petición que llega entre el clic y su llegada convierte «esta
    /// fila» en otra — y aquí «esta fila» es de quién se deshace el trabajo.
    generacion: u64,
    /// Cuántas sesiones se han olvidado por el tope.
    olvidadas: u64,
    /// Las sesiones con un deshacer en marcha.
    deshaciendo: std::collections::HashSet<String>,
}

impl Agentes {
    /// Apunta que esta sesión pidió permiso para `op`.
    pub(crate) fn vista(&mut self, id: &str, op: &str) {
        self.reloj += 1;
        self.generacion += 1;
        let sello = self.reloj;
        let (pintable, hostil) = norte_frontend::display_name(op.as_bytes());
        let entrada = self
            .sesiones
            .entry(id.to_owned())
            .or_insert_with(|| Sesion {
                id: id.to_owned(),
                vistas: 0,
                aprobadas: 0,
                ultima: (String::new(), false),
                sello,
            });
        entrada.vistas = entrada.vistas.saturating_add(1);
        entrada.ultima = (clamp_display(pintable), hostil);
        entrada.sello = sello;
        self.podar();
    }

    /// Apunta que a esta sesión se le aprobó una op desde aquí.
    pub(crate) fn aprobada(&mut self, id: &str) {
        if let Some(s) = self.sesiones.get_mut(id) {
            s.aprobadas = s.aprobadas.saturating_add(1);
            self.generacion += 1;
        }
    }

    /// Apunta que a esta sesión se le lanzó un deshacer.
    pub(crate) fn deshaciendo(&mut self, id: &str) {
        self.deshaciendo.insert(id.to_owned());
        self.generacion += 1;
    }

    /// El deshacer de esta sesión terminó, como sea.
    pub(crate) fn deshecha(&mut self, id: &str) {
        if self.deshaciendo.remove(id) {
            self.generacion += 1;
        }
    }

    /// `true` si esta sesión ya tiene un deshacer en marcha.
    pub(crate) fn tiene_undo_vivo(&self, id: &str) -> bool {
        self.deshaciendo.contains(id)
    }

    /// Se olvida de alguna cuando sobran, y lo apunta.
    ///
    /// NO la más vieja a secas: el id de sesión lo elige el AGENTE, y nada le
    /// impide reconectarse ciento veintiocho veces con ids nuevos, cada uno
    /// pidiendo un permiso, para empujar fuera de la lista justo a la sesión
    /// cuyo trabajo alguien querría deshacer. Se olvida primero lo que nadie
    /// ha tocado —una sola petición y ninguna aprobación desde aquí—, nunca
    /// una con un deshacer en marcha, y el recuento de olvidadas VIAJA:
    /// una lista recortada que se presenta como completa es lo que convierte
    /// el ataque en «esa sesión no existe».
    fn podar(&mut self) {
        while self.sesiones.len() > MAX_SESIONES {
            let Some(vieja) = self
                .sesiones
                .values()
                .filter(|s| !self.deshaciendo.contains(&s.id))
                .min_by_key(|s| (s.aprobadas > 0 || s.vistas > 1, s.sello))
                .map(|s| s.id.clone())
            else {
                return;
            };
            self.sesiones.remove(&vieja);
            if self.elegida.as_deref() == Some(vieja.as_str()) {
                self.elegida = None;
            }
            self.olvidadas = self.olvidadas.saturating_add(1);
        }
    }

    /// Las sesiones en el orden en que se pintan: la más reciente primero.
    fn ordenadas(&self) -> Vec<&Sesion> {
        let mut v: Vec<&Sesion> = self.sesiones.values().collect();
        // Por sello descendente, y el id como desempate: dos sesiones no
        // pueden compartir sello, pero un orden que dependa del recorrido de
        // un `HashMap` hace que la lista baile entre repintados.
        v.sort_by(|a, b| b.sello.cmp(&a.sello).then_with(|| a.id.cmp(&b.id)));
        v
    }

    /// El id CRUDO de la sesión elegida.
    pub(crate) fn elegida(&self) -> Option<String> {
        match &self.elegida {
            // Por ID: si la fila se movió —o desapareció—, la selección la
            // sigue, y no se queda señalando a quien ocupó su hueco.
            Some(id) if self.sesiones.contains_key(id) => Some(id.clone()),
            _ => self.ordenadas().first().map(|s| s.id.clone()),
        }
    }

    /// Dónde cae la selección dentro de la lista pintada.
    fn indice(&self) -> usize {
        let orden = self.ordenadas();
        self.elegida
            .as_ref()
            .and_then(|id| orden.iter().position(|s| &s.id == id))
            .unwrap_or(0)
    }

    /// Mueve la selección dentro de la lista.
    pub(crate) fn mover(&mut self, delta: i64) {
        let orden = self.ordenadas();
        if orden.is_empty() {
            return;
        }
        let destino = i64::try_from(self.indice())
            .unwrap_or(0)
            .saturating_add(delta);
        let i = usize::try_from(destino.max(0))
            .unwrap_or(0)
            .min(orden.len() - 1);
        self.elegida = Some(orden[i].id.clone());
    }

    /// Pone la selección en una fila concreta (un click), si el clic habla de
    /// la lista que se estaba pintando.
    ///
    /// Fuera de generación NO se recorta ni se ignora: se rehúsa. Recortar
    /// sobre una lista que se movió es elegir por el lector, y aquí lo que se
    /// elige es de quién se deshace el trabajo.
    pub(crate) fn senalar(&mut self, fila: usize, generacion: u64) -> bool {
        if generacion != self.generacion {
            return false;
        }
        let orden = self.ordenadas();
        let Some(s) = orden.get(fila) else {
            return false;
        };
        self.elegida = Some(s.id.clone());
        true
    }

    /// Empieza sin selección: el panel se abre y se cierra, y la de la vez
    /// anterior describía una lista que puede haber cambiado entera.
    pub(crate) fn al_abrir(&mut self) {
        self.elegida = None;
    }

    /// La proyección del panel.
    ///
    /// `escucha` es si esta ventana está suscrita al canal de aprobaciones:
    /// una que no lo está —montada sin efectos— tiene la lista vacía POR ESO,
    /// y decir ahí «ningún agente ha pedido permiso» es afirmar algo que no
    /// puede saber.
    pub(crate) fn vista_de(&self, lang: norte_i18n::Lang, escucha: bool) -> AgentsView {
        AgentsView {
            rows: self
                .ordenadas()
                .into_iter()
                .map(|s| {
                    let (id, hostil) = norte_frontend::display_name(s.id.as_bytes());
                    AgentRowView {
                        session: clamp_display(id),
                        session_hostile: hostil,
                        undoing: self.deshaciendo.contains(&s.id),
                        counts: clamp_display(norte_i18n::ta_in(
                            lang,
                            "agents-counts",
                            &[
                                ("seen", &s.vistas.to_string()),
                                ("approved", &s.aprobadas.to_string()),
                            ],
                        )),
                        last_op: s.ultima.0.clone(),
                        last_op_hostile: s.ultima.1,
                    }
                })
                .collect(),
            cursor: self.indice() as u64,
            generation: self.generacion,
            forgotten: self.olvidadas,
            // Qué ES esta lista, dentro de la propia pantalla: las que ESTA
            // ventana ha visto pedir permiso, que no es el censo de agentes
            // del sistema. Sin decirlo, una lista vacía se lee como «ningún
            // agente ha tocado nada», que es una afirmación que esta ventana
            // no puede hacer.
            note: clamp_display(norte_i18n::t_in(lang, "agents-note")),
            empty: clamp_display(norte_i18n::t_in(
                lang,
                if escucha {
                    "agents-empty"
                } else {
                    "agents-not-listening"
                },
            )),
        }
    }
}
