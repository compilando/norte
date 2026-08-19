//! El CUERPO de la sesión de UI (L2): lo que el core guarda y no lee.
//!
//! El core almacena un documento opaco —`version`, `revision`, `body`— porque
//! [`Node`], [`SortSpec`] y [`ColumnId`] viven AQUÍ, y este crate depende de
//! `norte-proto` y no al revés (ADR 0058). Este módulo es la otra mitad: el
//! esquema de ese cuerpo, su versión, y los topes que impiden que una pantalla
//! guardada crezca sin fin.
//!
//! **Los topes son del cliente**, y están en el tipo y no en el llamante: un
//! tope que se descubre después es una migración, y uno que cada llamante
//! recorta a su manera es tres topes distintos.

use std::collections::{BTreeMap, BTreeSet};

use norte_proto::VPath;
use serde::{Deserialize, Serialize};

use crate::columns::ColumnId;
use crate::layout::{Node, SlotId};
use crate::sort::SortSpec;

/// Esquema del cuerpo. Lo posee este crate, no el wire: añadir un campo a
/// [`SlotState`] es subir ESTE número, no la versión del protocolo.
pub const SCHEMA_VERSION: u32 = 1;

/// Entradas de historial por hueco y por sentido.
pub const HISTORY_CAP: usize = 64;

/// Huecos huérfanos —los que ningún layout menciona— que se guardan.
pub const ORPHAN_CAP: usize = 128;

/// Edad a la que un huérfano se barre: treinta días en milisegundos.
pub const MAX_AGE_MS: u64 = 30 * 24 * 60 * 60 * 1000;

/// Por qué un cuerpo no se pudo leer.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// El cuerpo lo escribió un binario más nuevo. Se rehúsa entero: mejor
    /// arrancar de la configuración que interpretar campos que no son tuyos.
    #[error("la sesión es de la versión {version} y esta sabe {SCHEMA_VERSION}")]
    FromTheFuture {
        /// La versión que traía.
        version: u32,
    },
    /// No encaja con el esquema. El mensaje NO cita el contenido: un cuerpo de
    /// sesión lleva rutas, y una ruta no va a un log por un error de parseo.
    #[error("la sesión no encaja con el esquema ({reason})")]
    Malformed {
        /// Categoría y posición, nunca el valor que no encajó.
        reason: String,
    },
    /// El cuerpo parsea, pero una de sus disposiciones no se puede usar.
    ///
    /// Va aparte de [`Self::Malformed`] porque la causa es otra y la cura
    /// también: aquí el JSON estaba bien y lo que no vale es el árbol, así
    /// que quien lo escribió fue una versión de norte, no un editor de texto.
    #[error("la sesión trae una disposición inválida: {reason}")]
    BadLayout {
        /// Qué le pasa al árbol. Nunca el contenido de un hueco.
        reason: String,
    },
}

/// El estado de UN hueco: dónde está, cómo mira y por dónde ha pasado.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotState {
    /// Dónde está el hueco. `VPath` y jamás `String`: es el único campo de
    /// esta struct que no es un número ni un enum, y tiparlo como texto
    /// perdería un nombre que no es UTF-8 sin que ningún test lo notara
    /// (regla 1).
    pub path: VPath,
    /// Fila del cursor dentro del listado.
    #[serde(default)]
    pub cursor: u64,
    /// Historial hacia atrás, del más viejo al más reciente.
    #[serde(default)]
    pub back: Vec<VPath>,
    /// Historial hacia delante, del más viejo al más reciente.
    #[serde(default)]
    pub forward: Vec<VPath>,
    /// Orden del listado.
    #[serde(default, deserialize_with = "orden::deserialize")]
    pub sort: SortSpec,
    /// Columnas visibles, en su forma string estable (`name`, `attr:…`,
    /// `plugin:…/…`): la MISMA que la configuración, y no un segundo
    /// vocabulario que mantener.
    ///
    /// **Hoy viaja SIEMPRE vacío desde la TUI (#236)**, donde las columnas
    /// visibles son configuración por scheme y no estado por hueco. El campo
    /// existe porque un frontend que sí las tenga por hueco lo necesita, y
    /// porque quitarlo después costaría subir [`SCHEMA_VERSION`]; quien lo
    /// llene tiene que llenarlo en la captura, no aquí.
    #[serde(default, with = "columnas")]
    pub columns: Vec<ColumnId>,
    /// Si se ven los ocultos.
    #[serde(default)]
    pub show_hidden: bool,
    /// Cuándo se tocó por última vez (epoch ms). Lo escribe el cliente, como
    /// todos los topes: la barrida por edad necesita un reloj por el que
    /// barrer, y el core no lee este documento.
    #[serde(default)]
    pub touched_ms: u64,
}

/// La pantalla guardada: las disposiciones por nombre y el estado por hueco.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBody {
    /// Disposiciones por nombre. `default` es la que se aplica al arrancar.
    #[serde(default)]
    pub layouts: BTreeMap<String, Node>,
    /// Estado por hueco, indexado por [`SlotId`].
    #[serde(default)]
    pub slots: BTreeMap<u32, SlotState>,
}

impl SessionBody {
    /// Recorta la sesión a sus topes. Se llama al ESCRIBIR, que es donde
    /// crece.
    ///
    /// En este orden: el historial de cada hueco se recorta por el extremo
    /// VIEJO —lo que se tira es lo más lejano, no lo que acabas de andar—; los
    /// huecos que alguna disposición menciona se marcan intocables; de los
    /// demás se van primero los de más de [`MAX_AGE_MS`] y luego, si aún
    /// sobran, los que hace más que no se tocan hasta caber en [`ORPHAN_CAP`].
    ///
    /// Un hueco VISIBLE no lo barre ni la edad ni el tope: lo que se ve en
    /// pantalla no se recicla.
    pub fn prune(&mut self, now_ms: u64) {
        for slot in self.slots.values_mut() {
            recorta_historial(&mut slot.back);
            recorta_historial(&mut slot.forward);
        }
        let visibles: BTreeSet<u32> = self
            .layouts
            .values()
            .flat_map(Node::slot_ids)
            .map(|SlotId(id)| id)
            .collect();
        self.slots.retain(|id, s| {
            visibles.contains(id) || now_ms.saturating_sub(s.touched_ms) <= MAX_AGE_MS
        });
        let mut huerfanos: Vec<(u64, u32)> = self
            .slots
            .iter()
            .filter(|(id, _)| !visibles.contains(*id))
            .map(|(id, s)| (s.touched_ms, *id))
            .collect();
        if huerfanos.len() <= ORPHAN_CAP {
            return;
        }
        // Por antigüedad de contacto: se van los de arriba, que son los que
        // hace más que nadie mira.
        huerfanos.sort_unstable();
        let sobran = huerfanos.len() - ORPHAN_CAP;
        for (_, id) in huerfanos.into_iter().take(sobran) {
            self.slots.remove(&id);
        }
    }

    /// El cuerpo como documento JSON, con su `version` dentro.
    ///
    /// # Panics
    ///
    /// Nunca: la struct es de tipos que serializan siempre, y el único mapa
    /// con clave no-string la tiene numérica.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).expect("SessionBody serializa siempre");
        if let Some(obj) = v.as_object_mut() {
            obj.insert(
                "version".to_owned(),
                serde_json::Value::from(SCHEMA_VERSION),
            );
        }
        v
    }

    /// Lee un cuerpo, comprobando la versión ANTES que la forma.
    ///
    /// # Errors
    ///
    /// [`SessionError::FromTheFuture`] si lo escribió un binario más nuevo, y
    /// [`SessionError::Malformed`] si no encaja con el esquema.
    pub fn from_value(v: &serde_json::Value) -> Result<Self, SessionError> {
        // Primero la versión: rehusar un cuerpo del futuro no puede depender
        // de que su forma le encaje a este binario.
        let version = v
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        if version > u64::from(SCHEMA_VERSION) {
            return Err(SessionError::FromTheFuture {
                version: u32::try_from(version).unwrap_or(u32::MAX),
            });
        }
        let cuerpo: Self =
            serde_json::from_value(v.clone()).map_err(|e| SessionError::Malformed {
                reason: diagnose(&e),
            })?;
        // Una disposición sin listado PARSEA —el esquema no la prohíbe— y
        // panicaba al aplicarse, en cada arranque mientras el fichero de
        // sesión siguiera ahí (#242). Se rechaza el cuerpo entero: el usuario
        // arranca de su configuración, que es reparable, en vez de de una
        // pantalla que no lo es.
        for (nombre, arbol) in &cuerpo.layouts {
            crate::layout::validate(arbol).map_err(|e| SessionError::BadLayout {
                reason: format!("{nombre}: {e}"),
            })?;
        }
        Ok(cuerpo)
    }
}

/// Qué toca hacer con la sesión en ESTE tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushStep {
    /// Nada: o no ha cambiado nada, o hay algo delante que dice que no es
    /// momento de guardar dónde estás.
    Skip,
    /// Preguntar si esta ventana ya puede escribir. Solo lo pide una ventana
    /// SUELTA, cada `retry_every` ticks.
    Ask,
    /// Capturar la pantalla y, si [`PushPolicy::prepare`] dice que ha
    /// cambiado, mandarla.
    Capture,
}

/// La política de escritura de la sesión: cuándo se manda, cuándo no se
/// repite, cuándo se vuelve a pedir la propiedad y qué se recorta antes de
/// mandar.
///
/// **Vive aquí y no en el frontend (#236).** El primer cliente de `session.put`
/// fue la TUI y toda esta política nació dentro de su bucle de eventos, con el
/// estado de recorte incluido; el segundo frontend la habría reimplementado
/// entera, bugs de recorte incluidos. Lo que NO está aquí es la fontanería:
/// los canales, el reloj y el `put` son de quien tenga runtime.
#[derive(Debug)]
pub struct PushPolicy {
    /// Lo último que se MANDÓ a escribir: se compara para no mandar dos veces
    /// lo mismo. Comparar el documento entero cuesta menos que un flag de
    /// sucio puesto a mano en los cientos de sitios que mueven un cursor —y no
    /// se puede olvidar en uno.
    last: Option<std::sync::Arc<SessionBody>>,
    /// Ticks que quedan para volver a preguntar por la propiedad.
    retry_in: u32,
    /// Cada cuántos ticks pregunta una ventana suelta.
    retry_every: u32,
}

impl PushPolicy {
    /// Una política que pregunta por la propiedad cada `retry_every` ticks.
    ///
    /// `retry_every` en 0 se trata como 1: preguntar «cada cero ticks» no es
    /// una cadencia, y la alternativa —no preguntar jamás— es el bug #234 otra
    /// vez.
    #[must_use]
    pub fn new(retry_every: u32) -> Self {
        let retry_every = retry_every.max(1);
        Self {
            last: None,
            retry_in: retry_every,
            retry_every,
        }
    }

    /// Qué toca este tick.
    ///
    /// `detached`: esta ventana no es la dueña, así que no escribe —pero sí
    /// vuelve a preguntar, porque la dueña pudo cerrarse hace un rato y de eso
    /// no avisa nadie (#234)—. `blocked`: hay algo delante (un modal) y la
    /// sesión trata de dónde estás, no de lo que estás decidiendo.
    pub fn tick(&mut self, detached: bool, blocked: bool) -> PushStep {
        if detached {
            self.retry_in = self.retry_in.saturating_sub(1);
            if self.retry_in == 0 {
                self.retry_in = self.retry_every;
                return PushStep::Ask;
            }
            return PushStep::Skip;
        }
        if blocked {
            return PushStep::Skip;
        }
        PushStep::Capture
    }

    /// Recorta el cuerpo a sus topes y, si ha cambiado desde lo último
    /// mandado, sella los huecos VIVOS que se movieron.
    ///
    /// `None` es «no ha cambiado»: este tick no manda nada. `Some(sellados)`
    /// son los huecos que el llamante tiene que sellar TAMBIÉN en su propio
    /// estado, con este mismo `now_ms` — el sello no puede salir de la captura
    /// porque hace falta saber contra qué comparar.
    ///
    /// Y solo los vivos: sellar también los huérfanos les devolvería la
    /// juventud en cada arranque y la barrida por edad no barrería nunca.
    pub fn prepare(
        &self,
        body: &mut SessionBody,
        live: &[SlotId],
        now_ms: u64,
    ) -> Option<Vec<SlotId>> {
        body.prune(now_ms);
        if self.last.as_deref() == Some(&*body) {
            return None;
        }
        let mut sellados = Vec::new();
        for id in live {
            let Some(estado) = body.slots.get_mut(&id.0) else {
                continue;
            };
            if self
                .last
                .as_deref()
                .and_then(|b| b.slots.get(&id.0))
                .is_some_and(|antes| antes == estado)
            {
                continue;
            }
            estado.touched_ms = now_ms;
            sellados.push(*id);
        }
        Some(sellados)
    }

    /// El cuerpo se mandó de verdad. Solo entonces cuenta como escrito: darlo
    /// por mandado cuando el canal estaba lleno pierde ese cuerpo para
    /// siempre.
    pub fn sent(&mut self, body: std::sync::Arc<SessionBody>) {
        self.last = Some(body);
    }

    /// Lo mandado NO llegó (otra ventana escribió antes): que la comparación
    /// no lo dé por escrito.
    pub fn resend(&mut self) {
        self.last = None;
    }

    /// Preguntar por la propiedad en el tick SIGUIENTE y no dentro de la
    /// cadencia entera: tras un relevo de daemon la sesión suele estar ya
    /// libre.
    pub fn ask_soon(&mut self) {
        self.retry_in = 1;
    }
}

/// El error de serde SIN su mensaje: su `Display` cita el valor que no encajó,
/// y ese valor sale de un documento que lleva rutas.
fn diagnose(e: &serde_json::Error) -> String {
    let que = match e.classify() {
        serde_json::error::Category::Io => "i/o",
        serde_json::error::Category::Syntax => "JSON mal formado",
        serde_json::error::Category::Data => "forma inesperada",
        serde_json::error::Category::Eof => "se acaba antes de tiempo",
    };
    format!("{que} en línea {} columna {}", e.line(), e.column())
}

/// Deja las [`HISTORY_CAP`] entradas más RECIENTES, que son las del final.
fn recorta_historial(h: &mut Vec<VPath>) {
    if h.len() > HISTORY_CAP {
        h.drain(..h.len() - HISTORY_CAP);
    }
}

/// Las columnas viajan por su forma string estable, la misma que la config:
/// [`ColumnId`] no tiene serde propio a propósito —su forma canónica es
/// `Display`/`FromStr`, con round-trip pineado— y darle una segunda aquí sería
/// un segundo vocabulario que mantener.
mod columnas {
    use serde::{Deserialize as _, Deserializer, Serializer};

    use crate::columns::ColumnId;

    /// Cada columna como su string canónico.
    pub(super) fn serialize<S: Serializer>(v: &[ColumnId], s: S) -> Result<S::Ok, S::Error> {
        s.collect_seq(v.iter().map(ToString::to_string))
    }

    /// Una columna que este binario no sabe leer se DESCARTA, no rompe el
    /// cuerpo entero: es una columna de menos en un panel, y el resto de la
    /// pantalla —rutas, historial, disposición— vale igual.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<ColumnId>, D::Error> {
        let raw = Vec::<String>::deserialize(d)?;
        Ok(raw.iter().filter_map(|s| s.parse().ok()).collect())
    }
}

/// El orden se lee TOLERANTE, por la misma razón que las columnas: una
/// columna de orden que este binario no conoce —`extension`, cuando la
/// añadan— es una preferencia de un panel, y hacerla fatal tiraría la pantalla
/// ENTERA (disposición, rutas e historial de todos los huecos) por ella. Sin
/// esto, añadir una variante a [`crate::sort::SortColumn`] sería un cambio de
/// [`SCHEMA_VERSION`], que es justo lo que este esquema dice que no cuesta.
mod orden {
    use serde::{Deserialize as _, Deserializer};

    use crate::sort::SortSpec;

    /// Un orden que no se entiende es el orden por defecto, no un cuerpo roto.
    pub(super) fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<SortSpec, D::Error> {
        let raw = serde_json::Value::deserialize(d)?;
        Ok(serde_json::from_value(raw).unwrap_or_default())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use norte_proto::Segment;

    use crate::layout::KindId;

    fn vp(s: &str) -> VPath {
        VPath::parse(s).expect("vpath")
    }

    fn slot(path: &str) -> SlotState {
        SlotState {
            path: vp(path),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            sort: SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
        }
    }

    /// Una disposición sin listado PARSEA, y al aplicarse dejaba la TUI sin
    /// panel al que apuntar: panic en modo raw, en cada arranque, hasta
    /// borrar el fichero de sesión a mano (#242). Se rechaza al leer.
    #[test]
    fn una_sesion_con_una_disposicion_sin_listado_no_se_lee() {
        let sin_listado = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                crate::layout::Node::slot(crate::layout::SlotId(1), KindId::new("places")),
                crate::layout::Node::slot(crate::layout::SlotId(4), KindId::new("status")),
            ],
        );
        let body = SessionBody {
            layouts: std::iter::once(("default".to_owned(), sin_listado)).collect(),
            slots: std::collections::BTreeMap::new(),
        };
        let v = body.to_value();
        assert!(matches!(
            SessionBody::from_value(&v),
            Err(SessionError::BadLayout { .. })
        ));
    }

    /// #236: la cadencia de una ventana SUELTA es de la política.
    ///
    /// Una suelta no escribe nunca, pero pregunta cada `retry_every` ticks: la
    /// dueña pudo cerrarse hace un rato y de eso no avisa nadie (#234).
    #[test]
    fn una_ventana_suelta_pregunta_con_cadencia_y_no_escribe() {
        let mut p = PushPolicy::new(3);
        assert_eq!(p.tick(true, false), PushStep::Skip);
        assert_eq!(p.tick(true, false), PushStep::Skip);
        assert_eq!(p.tick(true, false), PushStep::Ask, "al tercero pregunta");
        assert_eq!(p.tick(true, false), PushStep::Skip, "y vuelve a contar");

        // Tras un relevo de daemon la sesión ya suele estar libre: se pregunta
        // en el tick siguiente, no dentro de la cadencia entera.
        p.ask_soon();
        assert_eq!(p.tick(true, false), PushStep::Ask);
    }

    /// Con un modal delante no se guarda: la sesión trata de dónde estás, no
    /// de lo que estás decidiendo. Y sin nada delante, toca capturar.
    #[test]
    fn un_modal_tapa_la_escritura_y_nada_mas_la_deja_pasar() {
        let mut p = PushPolicy::new(3);
        assert_eq!(p.tick(false, true), PushStep::Skip);
        assert_eq!(p.tick(false, false), PushStep::Capture);
    }

    /// Sellar es de la política, y sella SOLO los huecos vivos que cambiaron:
    /// sellar un huérfano le devuelve la juventud en cada arranque y la
    /// barrida por edad no barre nunca.
    #[test]
    fn se_sellan_los_vivos_que_cambiaron_y_nadie_mas() {
        let mut p = PushPolicy::new(3);
        let mut body = SessionBody::default();
        body.slots.insert(1, slot("file:///uno"));
        body.slots.insert(2, slot("file:///dos"));
        // El 9 es huérfano: ninguna disposición lo menciona y no está en
        // `vivos`. Nace con un sello viejo para que la barrida no se lo lleve.
        let mut viejo = slot("file:///nueve");
        viejo.touched_ms = 1_000;
        body.slots.insert(9, viejo);

        let vivos = [SlotId(1), SlotId(2)];
        let sellados = p.prepare(&mut body, &vivos, 5_000).expect("es la primera");
        assert_eq!(sellados, vec![SlotId(1), SlotId(2)]);
        assert_eq!(body.slots[&1].touched_ms, 5_000);
        assert_eq!(
            body.slots[&9].touched_ms, 1_000,
            "el huérfano no rejuvenece"
        );

        // Mandado. El mismo cuerpo otra vez no se manda dos veces.
        p.sent(std::sync::Arc::new(body.clone()));
        let mut igual = body.clone();
        assert!(
            p.prepare(&mut igual, &vivos, 6_000).is_none(),
            "lo mismo no se repite"
        );
        assert_eq!(igual.slots[&1].touched_ms, 5_000, "ni se resella");

        // Mueve UNO: se sella ese y no el otro.
        let mut movido = body.clone();
        movido.slots.get_mut(&2).expect("el dos").cursor = 7;
        let sellados = p.prepare(&mut movido, &vivos, 7_000).expect("ha cambiado");
        assert_eq!(sellados, vec![SlotId(2)]);
        assert_eq!(movido.slots[&1].touched_ms, 5_000, "el quieto no se toca");
    }

    /// Lo que no llegó vuelve a mandarse: si `resend` no borrara el último,
    /// la comparación daría por escrito un cuerpo que otra ventana pisó.
    #[test]
    fn lo_que_no_llego_se_vuelve_a_mandar() {
        let mut p = PushPolicy::new(3);
        let mut body = SessionBody::default();
        body.slots.insert(1, slot("file:///uno"));
        p.prepare(&mut body, &[SlotId(1)], 5_000).expect("primera");
        p.sent(std::sync::Arc::new(body.clone()));
        assert!(p.prepare(&mut body.clone(), &[SlotId(1)], 6_000).is_none());

        p.resend();
        assert!(
            p.prepare(&mut body, &[SlotId(1)], 6_000).is_some(),
            "tras un conflicto se vuelve a mandar"
        );
    }

    /// Una cadencia de cero no es una cadencia: se trata como 1. Lo otro sería
    /// no preguntar jamás, que es el #234 otra vez.
    #[test]
    fn una_cadencia_de_cero_pregunta_cada_tick() {
        let mut p = PushPolicy::new(0);
        assert_eq!(p.tick(true, false), PushStep::Ask);
        assert_eq!(p.tick(true, false), PushStep::Ask);
    }

    /// Round trip por JSON: lo que sale es lo que entró.
    #[test]
    fn round_trip_por_json() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.columns = vec![
            "name".parse().expect("name"),
            "attr:posix.mode".parse().expect("attr"),
        ];
        s.cursor = 12;
        s.show_hidden = true;
        b.slots.insert(1, s);
        let vuelta = SessionBody::from_value(&b.to_value()).expect("parsea");
        assert_eq!(vuelta, b);
    }

    /// **La prueba que la regla 1 pide**: un nombre que no es UTF-8 sobrevive
    /// entero. Este es el sitio exacto donde un `String` se lo habría comido.
    #[test]
    fn un_nombre_no_utf8_sobrevive_al_viaje() {
        for name in norte_testkit::corpus::hostile_names() {
            let seg = Segment::new(name.bytes.clone()).expect("segmento");
            let ruta = vp("file:///casa").join(seg);
            let mut b = SessionBody::default();
            b.slots.insert(
                1,
                SlotState {
                    path: ruta.clone(),
                    ..slot("file:///casa")
                },
            );
            let vuelta = SessionBody::from_value(&b.to_value()).expect("parsea");
            assert_eq!(
                vuelta.slots[&1].path.file_name().map(Segment::as_bytes),
                ruta.file_name().map(Segment::as_bytes),
                "{} no sobrevivió",
                name.id
            );
            assert_eq!(vuelta.slots[&1].path, ruta, "{}", name.id);
        }
    }

    /// Un kind que este binario no declara vuelve intacto, `params` incluidos:
    /// la sesión guarda el árbol, no lo interpreta (ADR 0058).
    #[test]
    fn un_kind_desconocido_vuelve_entero() {
        let mut params = crate::layout::Params::new();
        params.set("grados", serde_json::json!(3));
        params.set("lo_que_sea", serde_json::json!({ "x": [1, 2] }));
        // Con un listado al lado: un árbol que no tiene ninguno no se lee
        // (#242), y lo que este test fija es que el kind AJENO vuelve intacto.
        let arbol = Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::Slot {
                    id: SlotId(4),
                    kind: KindId::new("kind-de-otro-binario"),
                    params,
                    bindings: crate::layout::Bindings::default(),
                },
            ],
        );
        let mut b = SessionBody::default();
        b.layouts.insert("default".into(), arbol.clone());
        let vuelta = SessionBody::from_value(&b.to_value()).expect("parsea");
        assert_eq!(vuelta.layouts["default"], arbol);
    }

    /// El historial se recorta al ESCRIBIR, y por el extremo viejo: lo que se
    /// tira es lo más lejano, no lo que acabas de andar.
    #[test]
    fn el_historial_se_recorta_por_lo_viejo() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.back = (0..HISTORY_CAP + 10)
            .map(|i| vp(&format!("file:///d{i}")))
            .collect();
        b.slots.insert(1, s);
        b.prune(0);
        let back = &b.slots[&1].back;
        assert_eq!(back.len(), HISTORY_CAP);
        assert_eq!(
            back.last().expect("último"),
            &vp(&format!("file:///d{}", HISTORY_CAP + 9))
        );
    }

    /// Un reloj de verdad, y no el cero: con `now_ms == 0` la barrida por edad
    /// no barre NADA, así que un test que pode en el cero no prueba que el
    /// huérfano sobreviva — prueba que la resta no llegó a hacerse.
    const AHORA: u64 = 1_750_000_000_000;

    /// Un layout que no menciona un hueco NO borra su estado: cambiar de
    /// disposición no te tira el historial.
    #[test]
    fn el_estado_huerfano_sobrevive_al_cambio_de_layout() {
        let mut b = SessionBody::default();
        let mut huerfano = slot("file:///lejos");
        huerfano.touched_ms = AHORA;
        b.slots.insert(7, huerfano);
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(AHORA);
        assert!(b.slots.contains_key(&7), "el huérfano se queda");
    }

    /// Y el que NADIE ha tocado nunca —`touched_ms` a cero contra un reloj de
    /// verdad— se va: sin sellar la marca al capturar, esto se lleva por
    /// delante todos los huérfanos en el primer volcado.
    #[test]
    fn un_huerfano_sin_sellar_se_barre_contra_un_reloj_de_verdad() {
        let mut b = SessionBody::default();
        b.slots.insert(7, slot("file:///lejos"));
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(AHORA);
        assert!(!b.slots.contains_key(&7), "sin sello no hay edad que valga");
    }

    /// Los huérfanos tienen tope, y cae el que hace más que no se toca.
    #[test]
    fn los_huerfanos_tienen_tope_y_cae_el_mas_viejo() {
        let mut b = SessionBody::default();
        let cap = u32::try_from(ORPHAN_CAP).expect("cabe");
        for i in 0..cap + 5 {
            let mut s = slot("file:///casa");
            s.touched_ms = u64::from(i);
            b.slots.insert(i, s);
        }
        b.prune(1_000);
        assert_eq!(b.slots.len(), ORPHAN_CAP);
        assert!(!b.slots.contains_key(&0), "el más viejo se fue");
        assert!(b.slots.contains_key(&(cap + 4)));
    }

    /// Y una edad: treinta días sin tocarse y el hueco se va, aunque quepa.
    #[test]
    fn un_hueco_de_hace_treinta_dias_se_barre() {
        let mut b = SessionBody::default();
        let mut viejo = slot("file:///casa");
        viejo.touched_ms = 0;
        let mut nuevo = slot("file:///casa");
        nuevo.touched_ms = MAX_AGE_MS;
        b.slots.insert(1, viejo);
        b.slots.insert(2, nuevo);
        b.prune(MAX_AGE_MS + 1);
        assert!(!b.slots.contains_key(&1));
        assert!(b.slots.contains_key(&2));
    }

    /// Un hueco que el layout VIVO menciona no lo barre ni la edad ni el tope:
    /// lo que se ve en pantalla no se recicla.
    #[test]
    fn un_hueco_visible_no_se_barre_jamas() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.touched_ms = 0;
        b.slots.insert(1, s);
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(AHORA);
        assert!(b.slots.contains_key(&1));
    }

    /// Una columna de ORDEN que este binario no conoce no tira la pantalla
    /// entera: se cae al orden por defecto y vuelve todo lo demás.
    ///
    /// La fixture era `extension` hasta que #138 la construyó, que es
    /// exactamente el caso que esta tolerancia existe para cubrir: la columna
    /// hipotética de ayer es la real de hoy, y un binario viejo tiene que
    /// seguir abriendo la sesión que escribió uno nuevo.
    #[test]
    fn una_columna_de_orden_desconocida_no_tira_el_cuerpo() {
        let v = serde_json::json!({
            "version": SCHEMA_VERSION,
            "layouts": {},
            "slots": { "1": {
                "path": "file:///casa",
                "back": ["file:///antes"],
                "sort": { "column": "creacion", "dir": "asc", "dirs_first": true },
            }},
        });
        let b = SessionBody::from_value(&v).expect("parsea");
        assert_eq!(b.slots[&1].sort, SortSpec::default());
        assert_eq!(b.slots[&1].path, vp("file:///casa"));
        assert_eq!(
            b.slots[&1].back,
            vec![vp("file:///antes")],
            "y el historial"
        );
    }

    /// Un cuerpo de una versión que este binario no conoce se rehúsa: mejor
    /// arrancar de la config que interpretar campos que no son los tuyos.
    #[test]
    fn un_esquema_del_futuro_se_rehusa() {
        let v = serde_json::json!({ "version": SCHEMA_VERSION + 1, "layouts": {}, "slots": {} });
        assert!(matches!(
            SessionBody::from_value(&v),
            Err(SessionError::FromTheFuture { .. })
        ));
    }

    /// Una columna que este binario no sabe leer no se lleva por delante la
    /// pantalla entera: se descarta ella y el resto vuelve.
    #[test]
    fn una_columna_desconocida_se_descarta_sin_tirar_el_cuerpo() {
        let v = serde_json::json!({
            "version": SCHEMA_VERSION,
            "layouts": {},
            "slots": { "1": {
                "path": "file:///casa",
                "columns": ["name", "columna-de-otro-binario"],
            }},
        });
        let b = SessionBody::from_value(&v).expect("parsea");
        assert_eq!(b.slots[&1].columns, vec!["name".parse().expect("name")]);
        assert_eq!(b.slots[&1].path, vp("file:///casa"));
    }
}
