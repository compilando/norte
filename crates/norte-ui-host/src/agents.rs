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
    /// Cuál está elegida, cuando el panel está abierto.
    cursor: usize,
}

impl Agentes {
    /// Apunta que esta sesión pidió permiso para `op`.
    pub(crate) fn vista(&mut self, id: &str, op: &str) {
        self.reloj += 1;
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
        }
    }

    /// Se olvida de la vista hace más tiempo cuando sobra alguna.
    fn podar(&mut self) {
        while self.sesiones.len() > MAX_SESIONES {
            let Some(vieja) = self
                .sesiones
                .values()
                .min_by_key(|s| s.sello)
                .map(|s| s.id.clone())
            else {
                return;
            };
            self.sesiones.remove(&vieja);
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
        self.ordenadas().get(self.cursor).map(|s| s.id.clone())
    }

    /// Mueve el cursor dentro de la lista.
    pub(crate) fn mover(&mut self, delta: i64) {
        let n = self.sesiones.len();
        if n == 0 {
            return;
        }
        let destino = i64::try_from(self.cursor)
            .unwrap_or(0)
            .saturating_add(delta);
        self.cursor = usize::try_from(destino.max(0)).unwrap_or(0).min(n - 1);
    }

    /// Pone el cursor en una fila concreta (un click).
    pub(crate) fn senalar(&mut self, fila: usize) {
        if fila < self.sesiones.len() {
            self.cursor = fila;
        }
    }

    /// Empieza en la primera: el panel se abre y se cierra, y el cursor de la
    /// vez anterior describía una lista que puede haber cambiado entera.
    pub(crate) fn al_abrir(&mut self) {
        self.cursor = 0;
    }

    /// La proyección del panel.
    pub(crate) fn vista_de(&self, lang: norte_i18n::Lang) -> AgentsView {
        AgentsView {
            rows: self
                .ordenadas()
                .into_iter()
                .map(|s| {
                    let (id, hostil) = norte_frontend::display_name(s.id.as_bytes());
                    AgentRowView {
                        session: clamp_display(id),
                        session_hostile: hostil,
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
            cursor: self.cursor as u64,
            // Qué ES esta lista, dentro de la propia pantalla: las que ESTA
            // ventana ha visto pedir permiso, que no es el censo de agentes
            // del sistema. Sin decirlo, una lista vacía se lee como «ningún
            // agente ha tocado nada», que es una afirmación que esta ventana
            // no puede hacer.
            note: clamp_display(norte_i18n::t_in(lang, "agents-note")),
        }
    }
}
