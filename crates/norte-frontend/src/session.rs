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
pub const SCHEMA_VERSION: u32 = 2;

/// Entradas de historial por hueco y por sentido.
pub const HISTORY_CAP: usize = 64;

/// Huecos huérfanos —los que ningún layout menciona— que se guardan.
pub const ORPHAN_CAP: usize = 128;

/// Entradas de historial que conserva un hueco HUÉRFANO, por sentido.
///
/// Un hueco que ninguna disposición menciona no está en pantalla: nadie puede
/// pulsar «atrás» dentro de él sin volver a abrirlo antes, y volver a abrirlo
/// es empezar a andar de nuevo. Los 64 pasos de [`HISTORY_CAP`] son para el
/// hueco que se ve.
///
/// El número sale de una ARITMÉTICA, no del gusto (#304): [`ORPHAN_CAP`] es
/// 128 y un hueco con el historial lleno mide ~9 240 bytes con rutas de este
/// repositorio, así que 128 huérfanos por sí solos daban ~1 182 000 contra los
/// 1 048 576 de [`norte_proto::methods::SESSION_BODY_MAX`] — un cuerpo que el
/// core REHÚSA, dejando la sesión como estaba. `prune` recorta contra cuentas
/// y el tope real es de bytes; bajar el historial del que nadie mira es lo que
/// devuelve el sentido al tope por cuenta.
/// `el_tope_de_huerfanos_lleno_tambien_cabe_en_el_sobre` mide las dos cotas a
/// la vez; si se pone rojo, la cura es BAJAR este número.
pub const ORPHAN_HISTORY_CAP: usize = 8;

/// Cuántos PERFILES conservan estado a la vez (spec 2026-08-26, D6).
///
/// El número sale de una MEDIDA, no del gusto:
/// `un_cuerpo_realista_con_el_tope_lleno_cabe_en_el_sobre` serializa cuatro
/// perfiles de ocho huecos con el historial lleno en los dos sentidos y rutas
/// de este repositorio, y da **295 567 bytes** contra los 1 048 576 de
/// [`norte_proto::methods::SESSION_BODY_MAX`] — 28 % del sobre, con sitio para
/// que las rutas de otro sean bastante más largas que las de aquí. Si ese test
/// se pone rojo, la cura es BAJAR este número: el core rehúsa un `put` que se
/// pase y deja la sesión como estaba, así que pasarse es perder lo que estabas
/// haciendo.
///
/// Pasado el tope se va el estado del perfil que hace más que nadie activa,
/// ENTERO. Su directorio de configuración no se toca: el perfil sigue
/// existiendo y su próximo arranque sale de `[profile.start]`.
pub const PROFILE_STATE_CAP: usize = 4;

/// Sufijo de la clave de `layouts` bajo la que la VENTANA guarda su
/// disposición (ADR 0139): `default@window`, `<perfil>@window`.
///
/// La terminal y la ventana recuerdan cada una la suya —tamaños y
/// posiciones de los paneles—, porque compartirla hacía que la última en
/// escribir pisara lo que la otra había ajustado. La clave de la terminal
/// sigue siendo el nombre del perfil a secas.
pub const WINDOW_LAYOUT_SUFFIX: &str = "@window";

/// La clave de la ventana para el perfil de clave `perfil`.
#[must_use]
pub fn window_layout_key(perfil: &str) -> String {
    format!("{perfil}{WINDOW_LAYOUT_SUFFIX}")
}

/// El perfil al que pertenece una clave de `layouts`: la de la ventana
/// cuenta como la de su perfil para podar, y se poda con él.
fn perfil_de(clave: &str) -> &str {
    clave.strip_suffix(WINDOW_LAYOUT_SUFFIX).unwrap_or(clave)
}

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
    /// El punto de salto del hueco (`nav.set-jump-point`, spec 2026-09-15
    /// D5). Aditivo: un cuerpo viejo lo lee vacío y no sube
    /// [`SCHEMA_VERSION`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jump: Option<VPath>,
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
    /// Lo MARCADO en este hueco, a lo sumo [`MARKS_CAP`] (fase 9).
    ///
    /// Las marcas son lo único de la pantalla que no sobrevivía a un relevo
    /// entre frontends, y es justo lo que más caro cuesta rehacer: recuperar
    /// un directorio y un cursor es un `cd`; recuperar cuarenta ficheros
    /// señalados a mano es volver a señalarlos.
    ///
    /// **Son `VPath`, o sea la IDENTIDAD de la fila, y jamás su índice.** Una
    /// lista que se reordena o que pierde una vecina por encima deja un índice
    /// apuntando a otro fichero, y lo que se restauraría sería una selección
    /// que nadie hizo — sobre la que después se pulsa borrar. Es la misma
    /// razón por la que las marcas vivas se guardan por ruta.
    ///
    /// Aditivo: un cuerpo viejo lo lee vacío y no sube [`SCHEMA_VERSION`],
    /// igual que `jump` o `palette_recent`. Y se omite si está vacío, que es
    /// lo normal: un hueco sin marcas produce los MISMOS bytes que antes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub marks: Vec<VPath>,
}

/// La pantalla guardada: las disposiciones por nombre y el estado por hueco.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionBody {
    /// El perfil activo. Vacío = ninguno.
    ///
    /// Es ESTADO, no configuración: lo que estabas haciendo, no lo que
    /// decidiste. Por eso vive aquí y no en el `norte.toml` del lector, que
    /// sigue siendo un fichero que escribió él.
    ///
    /// Es `String` y no `OsString` porque es la CLAVE de [`Self::layouts`],
    /// que es un objeto JSON y por tanto UTF-8 por construcción. Un perfil
    /// cuyo directorio no sea UTF-8 vale para configuración y no puede llevar
    /// estado, tampoco pegajoso (spec 2026-08-26, D4).
    #[serde(default)]
    pub active: String,
    /// Disposiciones por nombre DE PERFIL. Vacío o `default` es la del lector
    /// sin perfil; con [`Self::active`] puesto, la clave es ese nombre.
    #[serde(default)]
    pub layouts: BTreeMap<String, Node>,
    /// Estado por hueco, indexado por [`SlotId`].
    #[serde(default)]
    pub slots: BTreeMap<u32, SlotState>,
    /// Las últimas claves de despacho lanzadas desde la paleta, la más
    /// reciente primero, a lo sumo [`PALETTE_RECENT_CAP`] (spec 2026-09-10).
    /// Es ESTADO, como el perfil activo: lo que hiciste, no lo que
    /// decidiste. Un campo con `default` es aditivo: un cuerpo viejo lo lee
    /// vacío y uno nuevo lo escribe; no sube [`SCHEMA_VERSION`].
    #[serde(default)]
    pub palette_recent: Vec<String>,
    /// Los directorios populares de la sesión entera
    /// ([`crate::history::Popular`], spec 2026-09-15 D6), en el orden en que
    /// se guardan. Aditivo como [`Self::palette_recent`].
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "populares::deserialize"
    )]
    pub popular: Vec<crate::history::PopularEntry>,
}

/// Los populares se leen ENTRADA A ENTRADA.
///
/// Una ruta que no parsea —un cuerpo editado a mano— se salta en vez de
/// rehusar el cuerpo entero, que se llevaría por delante disposiciones y huecos
/// que no tienen nada que ver. Es una lista de atajos que se rehace andando
/// (spec 2026-09-15 D6); la ruta de un hueco, en cambio, sigue siendo un error,
/// porque sin ella el hueco no es nada.
mod populares {
    use serde::{Deserialize, Deserializer};

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Vec<crate::history::PopularEntry>, D::Error> {
        let crudas = Vec::<serde_json::Value>::deserialize(d)?;
        Ok(crudas
            .into_iter()
            .filter_map(|v| serde_json::from_value(v).ok())
            .collect())
    }
}

/// Cuántos comandos recientes guarda la paleta. Cinco: los que caben en la
/// vista sin empujar la lista entera bajo el borde.
pub const PALETTE_RECENT_CAP: usize = 5;

/// Marcas que conserva un hueco (fase 9, spec 2026-09-15).
///
/// El tope vive AQUÍ y no en cada llamante, por lo mismo que los demás: un
/// documento acotado en cinco sitios está acotado en cuatro. Cuatro mil
/// noventa y seis rutas de este repositorio rondan los 160 KiB —holgado bajo
/// el [`norte_proto::methods::SESSION_BODY_MAX`] de 1 MiB— y por encima de esa
/// cifra lo que hay no es una selección que un humano hizo a mano, sino un
/// «marcar todo» sobre un directorio enorme, que se rehace con una tecla.
pub const MARKS_CAP: usize = 4096;

/// Anota `key` como el comando más reciente de la paleta: lo pone primero,
/// quita su repetición anterior y recorta a [`PALETTE_RECENT_CAP`].
pub fn note_palette_recent(recent: &mut Vec<String>, key: &str) {
    recent.retain(|k| k != key);
    recent.insert(0, key.to_owned());
    recent.truncate(PALETTE_RECENT_CAP);
}

impl SessionBody {
    /// Recorta la sesión a sus topes. Se llama al ESCRIBIR, que es donde
    /// crece.
    ///
    /// En este orden: el tope de PERFILES con estado ([`PROFILE_STATE_CAP`],
    /// spec 2026-08-26, D6) primero, para que todo lo demás vea ya el mapa más
    /// pequeño; los huecos que alguna disposición menciona quedan marcados
    /// intocables; el historial de cada hueco se recorta por el extremo VIEJO
    /// —lo que se tira es lo más lejano, no lo que acabas de andar—, a
    /// [`HISTORY_CAP`] si el hueco es visible y a [`ORPHAN_HISTORY_CAP`] si no;
    /// y de los huérfanos se van primero los de más de [`MAX_AGE_MS`] y luego,
    /// si aún sobran, los que hace más que no se tocan hasta caber en
    /// [`ORPHAN_CAP`].
    ///
    /// Un hueco VISIBLE no lo barre ni la edad ni el tope, ni pierde un paso de
    /// historial: lo que se ve en pantalla no se recicla.
    ///
    /// El perfil [`Self::active`] no lo barre nada, en ningún paso.
    ///
    /// Y al final, el tope que de verdad manda: se mide el cuerpo SERIALIZADO
    /// y se sigue recortando hasta que quepa en
    /// [`norte_proto::methods::SESSION_BODY_MAX`]. Todos los de arriba son de
    /// CUENTAS y el del core es de BYTES, así que ninguna cuenta puede
    /// prometer que el cuerpo entre; el orden en que se degrada está declarado
    /// en `fit_to_envelope`, y lo que jamás se toca es el perfil activo, su
    /// disposición, y la ruta y el cursor de cada hueco visible.
    pub fn prune(&mut self, now_ms: u64) {
        self.prune_profiles();
        if self.popular.len() > crate::history::POPULAR_CAP {
            // La MISMA regla de expulsión que al visitar, y no un `truncate`:
            // el orden guardado no es el de importancia.
            self.popular = crate::history::Popular::from_entries(std::mem::take(&mut self.popular))
                .entries()
                .to_vec();
        }
        let visibles: BTreeSet<u32> = self
            .layouts
            .values()
            .flat_map(Node::slot_ids)
            .map(|SlotId(id)| id)
            .collect();
        for (id, slot) in &mut self.slots {
            let cap = if visibles.contains(id) {
                HISTORY_CAP
            } else {
                ORPHAN_HISTORY_CAP
            };
            recorta_historial(&mut slot.back, cap);
            recorta_historial(&mut slot.forward, cap);
        }
        self.slots.retain(|id, s| {
            visibles.contains(id) || now_ms.saturating_sub(s.touched_ms) <= MAX_AGE_MS
        });
        let mut huerfanos: Vec<(u64, u32)> = self
            .slots
            .iter()
            .filter(|(id, _)| !visibles.contains(*id))
            .map(|(id, s)| (s.touched_ms, *id))
            .collect();
        if huerfanos.len() > ORPHAN_CAP {
            // Por antigüedad de contacto: se van los de arriba, que son los
            // que hace más que nadie mira.
            huerfanos.sort_unstable();
            let sobran = huerfanos.len() - ORPHAN_CAP;
            for (_, id) in huerfanos.into_iter().take(sobran) {
                self.slots.remove(&id);
            }
        }
        self.fit_to_envelope(&visibles);
    }

    /// Recorta hasta que el cuerpo QUEPA de verdad, midiendo bytes.
    ///
    /// Todos los topes de [`Self::prune`] son de cuentas y el del core es de
    /// bytes ([`norte_proto::methods::SESSION_BODY_MAX`]), así que ninguna
    /// cuenta puede prometer que el cuerpo entre: las rutas las elige el
    /// lector. Un nombre no-UTF-8 viaja percent-encoded y mide el triple; un
    /// árbol profundo multiplica cada entrada del historial; y el número de
    /// huecos VISIBLES no tiene tope ninguno — nada impide veinte pestañas por
    /// perfil. Cuando el cuerpo se pasa, el core rehúsa el `put` ENTERO y la
    /// sesión almacenada se queda como estaba.
    ///
    /// El orden en que se degrada es el orden en que duele menos, y se declara
    /// aquí porque un recorte que el lector no puede predecir es peor que uno
    /// que sí:
    ///
    /// 1. los huérfanos, ENTEROS y del que hace más que no se toca hacia
    ///    delante — nadie los está mirando;
    /// 2. los populares, enteros — son atajos que se rehacen andando;
    /// 3. el historial de los visibles, a la mitad cada vuelta hasta cero — se
    ///    pierden pasos hacia atrás, no dónde estás;
    /// 4. las disposiciones de los perfiles que no son el activo, con sus
    ///    huecos, de la que hace más que nadie activa hacia delante.
    ///
    /// Lo que jamás se toca: el perfil [`Self::active`], su disposición, y la
    /// RUTA y el cursor de cada hueco visible. Si ni así cabe —un cuerpo con
    /// una sola disposición de rutas monstruosas— se manda lo que haya: el
    /// rechazo del core es honesto y el frontend lo dice, mientras que
    /// inventarse un recorte del árbol activo sería devolverle al lector una
    /// pantalla que él no dejó.
    fn fit_to_envelope(&mut self, visibles: &BTreeSet<u32>) {
        if self.cabe() {
            return;
        }
        let mut huerfanos: Vec<(u64, u32)> = self
            .slots
            .iter()
            .filter(|(id, _)| !visibles.contains(*id))
            .map(|(id, s)| (s.touched_ms, *id))
            .collect();
        huerfanos.sort_unstable();
        for (_, id) in huerfanos {
            self.slots.remove(&id);
            if self.cabe() {
                return;
            }
        }
        if !self.popular.is_empty() {
            self.popular.clear();
            if self.cabe() {
                return;
            }
        }
        let mut cap = HISTORY_CAP;
        while cap > 0 {
            cap /= 2;
            for slot in self.slots.values_mut() {
                recorta_historial(&mut slot.back, cap);
                recorta_historial(&mut slot.forward, cap);
            }
            if self.cabe() {
                return;
            }
        }
        // Del que hace más que nadie activa hacia delante, y el ACTIVO no está
        // en esta lista: es el único que no se puede tirar.
        for nombre in self.profiles_by_last_touch() {
            let arboles = self.quitar_perfil(&nombre);
            if arboles.is_empty() {
                continue;
            }
            let vivos: BTreeSet<u32> = self
                .layouts
                .values()
                .flat_map(Node::slot_ids)
                .map(|SlotId(id)| id)
                .collect();
            for SlotId(id) in arboles.iter().flat_map(Node::slot_ids) {
                if !vivos.contains(&id) {
                    self.slots.remove(&id);
                }
            }
            if self.cabe() {
                return;
            }
        }
    }

    /// Degrada el cuerpo para REINTENTAR un `put` que el core rehusó por
    /// tamaño, y dice si quedaba algo que tirar (#316).
    ///
    /// Tira los dos rastros de cada hueco, que es lo que más ocupa de una
    /// sesión y lo que menos duele perder: se pierden pasos hacia atrás, no
    /// dónde estás. Lo que jamás toca son las rutas, el cursor ni las
    /// disposiciones — un `put` rehusado deja la sesión ALMACENADA como estaba,
    /// así que el lector pierde su pantalla entera, y volver con el historial
    /// vacío es infinitamente mejor que volver a donde estaba hace una semana.
    ///
    /// `false` = ya no queda historial. Entonces reintentar es pedir el mismo
    /// error otra vez, y lo honesto es decir que no se guardó.
    ///
    /// Vive aquí y no en cada frontend porque es una DECISIÓN y no fontanería:
    /// la TUI la tomaba en su escritor y la ventana no la tomaba en absoluto
    /// —cualquier error de `session_put` era «no llegó», sin degradar y sin
    /// avisar—, que es exactamente la divergencia silenciosa del ADR 0077.
    /// La poda por bytes de [`Self::prune`] hace que esto casi nunca haga
    /// falta; casi.
    pub fn degrade_for_size(&mut self) -> bool {
        let mut habia = false;
        for slot in self.slots.values_mut() {
            habia |= !slot.back.is_empty() || !slot.forward.is_empty();
            slot.back.clear();
            slot.forward.clear();
        }
        habia
    }

    /// ¿Cabe este cuerpo en el sobre que el core acepta?
    ///
    /// Se mide serializando, que es lo único que contesta la pregunta de
    /// verdad — el core mide los bytes del `body`, no los elementos. Un fallo
    /// al serializar cuenta como que SÍ cabe: no serializar es un problema
    /// distinto, lo verá el `put`, y ponerse a recortar por ello tiraría estado
    /// bueno por una razón que no es esa.
    fn cabe(&self) -> bool {
        serde_json::to_vec(&self.to_value())
            .map_or(true, |b| b.len() <= norte_proto::methods::SESSION_BODY_MAX)
    }

    /// Los perfiles con estado que NO son el activo, del que hace más que nadie
    /// activa al más reciente.
    ///
    /// «Hace más que nadie lo activa» se DERIVA y no se guarda: es el perfil
    /// cuyo hueco tocado más recientemente lo fue antes que el de los demás.
    /// Sin campo nuevo y sin reloj — la misma disciplina que el orden de
    /// huérfanos de `SlotStore`, donde un reloj haría los tests dependientes
    /// del tiempo. A igualdad de toque, por nombre: la poda tiene que ser
    /// determinista y no depender del orden del mapa.
    fn profiles_by_last_touch(&self) -> Vec<String> {
        let ultimo_toque = |arbol: &Node| -> u64 {
            arbol
                .slot_ids()
                .into_iter()
                .filter_map(|SlotId(id)| self.slots.get(&id))
                .map(|s| s.touched_ms)
                .max()
                .unwrap_or(0)
        };
        // Por PERFIL, no por clave: la disposición de la ventana
        // (`<perfil>@window`) es del mismo perfil que la de la terminal, y
        // su toque cuenta para los dos.
        let mut toques: std::collections::BTreeMap<String, u64> = std::collections::BTreeMap::new();
        for (clave, arbol) in &self.layouts {
            let perfil = perfil_de(clave);
            if perfil == self.active {
                continue;
            }
            let t = toques.entry(perfil.to_owned()).or_insert(0);
            *t = (*t).max(ultimo_toque(arbol));
        }
        let mut orden: Vec<(u64, String)> = toques.into_iter().map(|(p, t)| (t, p)).collect();
        orden.sort_unstable();
        orden.into_iter().map(|(_, nombre)| nombre).collect()
    }

    /// Quita las disposiciones del perfil `perfil` —la de la terminal y la
    /// de la ventana— y las devuelve.
    fn quitar_perfil(&mut self, perfil: &str) -> Vec<Node> {
        let claves: Vec<String> = self
            .layouts
            .keys()
            .filter(|c| perfil_de(c) == perfil)
            .cloned()
            .collect();
        claves
            .iter()
            .filter_map(|c| self.layouts.remove(c))
            .collect()
    }

    /// Cuántos PERFILES tienen estado (la ventana no cuenta aparte).
    fn perfiles_con_estado(&self) -> usize {
        self.layouts
            .keys()
            .map(|c| perfil_de(c))
            .collect::<BTreeSet<_>>()
            .len()
    }

    /// Deja como mucho [`PROFILE_STATE_CAP`] perfiles con estado, tirando
    /// enteros los que hace más que nadie activa.
    ///
    /// «Hace más que nadie lo activa» se DERIVA y no se guarda: es el perfil
    /// cuyo hueco tocado más recientemente lo fue antes que el de los demás.
    /// Sin campo nuevo y sin reloj — la misma disciplina que el orden de
    /// huérfanos de `SlotStore`, donde un reloj haría los tests dependientes
    /// del tiempo.
    fn prune_profiles(&mut self) {
        let perfiles = self.perfiles_con_estado();
        if perfiles <= PROFILE_STATE_CAP {
            return;
        }
        let orden = self.profiles_by_last_touch();
        let sobran = perfiles - PROFILE_STATE_CAP;
        let mut candidatos: BTreeSet<u32> = BTreeSet::new();
        for nombre in orden.into_iter().take(sobran) {
            for arbol in self.quitar_perfil(&nombre) {
                candidatos.extend(arbol.slot_ids().into_iter().map(|SlotId(id)| id));
            }
        }
        // Los huecos del perfil que se va se borran contra LO QUE QUEDA, no a
        // ciegas por su árbol.
        //
        // Que dos perfiles no compartan hueco es un invariante del REPARTO
        // (`next_slot_base` + `rebase_slot_ids`), y nada lo impone sobre un
        // cuerpo que llega de disco: `from_value` valida cada árbol por
        // separado —duplicados DENTRO de uno— y no dice nada de un id
        // compartido entre DOS, y el cuerpo es opaco para el core, así que
        // cualquier cliente puede escribir uno así. Borrando a ciegas, un
        // cuerpo de ésos se llevaba por delante los huecos del perfil ACTIVO:
        // el lector perdía el directorio, el cursor y los dos rastros de los
        // paneles que estaba mirando, que es justo lo que el rustdoc de
        // `prune` promete que no pasa.
        //
        // Y solo se miran los ids del perfil saliente: los huérfanos de otros
        // NO se tocan aquí, que para eso está el barrido por edad de abajo.
        let vivos: BTreeSet<u32> = self
            .layouts
            .values()
            .flat_map(Node::slot_ids)
            .map(|SlotId(id)| id)
            .collect();
        for id in candidatos {
            if !vivos.contains(&id) {
                self.slots.remove(&id);
            }
        }
    }

    /// El primer id de hueco que no usa NADIE: ni una disposición de ningún
    /// perfil, ni un estado guardado, huérfanos incluidos.
    ///
    /// Es la base que [`Node::rebase_slot_ids`] necesita para que dos perfiles
    /// no compartan hueco (spec 2026-08-26, D5). Mira TODO y no solo el perfil
    /// activo a propósito: repartir contra lo que se ve en pantalla acabaría
    /// reasignando encima del estado guardado de otro perfil, que es
    /// justamente el estado que nadie está mirando cuando pasa.
    ///
    /// Una sesión vacía empieza en 1. `None` = no queda espacio: el id más
    /// alto en uso es `u32::MAX`, y no hay «el siguiente». Devolverlo saturado
    /// era decir que `u32::MAX` está libre teniéndolo ocupado, con el
    /// resultado de que [`Node::rebase_slot_ids`] repartía ese mismo número a
    /// todos los huecos del árbol.
    #[must_use]
    pub fn next_slot_base(&self) -> Option<u32> {
        let de_arboles = self
            .layouts
            .values()
            .flat_map(Node::slot_ids)
            .map(|SlotId(id)| id);
        let de_estados = self.slots.keys().copied();
        match de_arboles.chain(de_estados).max() {
            None => Some(1),
            Some(m) => m.checked_add(1),
        }
    }

    /// El cuerpo como documento JSON.
    ///
    /// **Sin `version` dentro** desde #247: el esquema del cuerpo lo declara
    /// [`norte_proto::methods::SessionPutParams::version`], que es el campo
    /// que el protocolo documenta y el único que el core mira. Había DOS, y
    /// el documentado no lo leía nadie — un cliente ajeno que hiciera lo que
    /// dice el contrato (poner un cuerpo v2 y `version: 2` en el sobre)
    /// llegaba a un lector que solo miraba la copia de dentro, la veía
    /// ausente, la tomaba por 0 y se comía los campos que no entendía.
    ///
    /// Un cuerpo escrito por una versión anterior SÍ trae la copia, y
    /// [`Self::from_value`] la sigue leyendo: quitarla de aquí no puede
    /// invalidar lo que ya está en disco.
    ///
    /// # Panics
    ///
    /// Nunca: la struct es de tipos que serializan siempre, y el único mapa
    /// con clave no-string la tiene numérica.
    #[must_use]
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("SessionBody serializa siempre")
    }

    /// Lee un cuerpo, comprobando la versión ANTES que la forma.
    ///
    /// `envelope` es la versión que declara el SOBRE
    /// ([`norte_proto::methods::Session::version`]), que es la que el
    /// protocolo documenta. Manda la MAYOR de las dos —el sobre y la copia
    /// que los cuerpos antiguos llevan dentro—, porque las dos son una
    /// afirmación de quién lo escribió y rehusar es lo seguro: leer un cuerpo
    /// más nuevo del que se entiende y volver a escribirlo pierde campos en
    /// silencio, que es lo que ADR 0059 promete que no pasa (#247).
    ///
    /// # Errors
    ///
    /// [`SessionError::FromTheFuture`] si lo escribió un binario más nuevo,
    /// [`SessionError::Malformed`] si no encaja con el esquema y
    /// [`SessionError::BadLayout`] si trae una disposición inservible.
    pub fn from_value(envelope: u32, v: &serde_json::Value) -> Result<Self, SessionError> {
        // Primero la versión: rehusar un cuerpo del futuro no puede depender
        // de que su forma le encaje a este binario.
        let dentro = v
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(0);
        let version = dentro.max(u64::from(envelope));
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

/// Deja las `cap` entradas más RECIENTES, que son las del final.
fn recorta_historial(h: &mut Vec<VPath>, cap: usize) {
    if h.len() > cap {
        h.drain(..h.len() - cap);
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
            jump: None,
            sort: SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        }
    }

    /// Un cuerpo con un perfil por nombre, cada uno con dos huecos propios y
    /// su `touched_ms`, que es lo que ordena «hace más que nadie lo activa».
    fn cuerpo_con_perfiles(perfiles: &[(&str, u64)]) -> SessionBody {
        let fabrica = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let mut b = SessionBody::default();
        for (nombre, tocado) in perfiles {
            let (arbol, _) = fabrica.rebase_slot_ids(b.next_slot_base().expect("hay sitio"));
            for SlotId(id) in arbol.slot_ids() {
                let mut s = slot("file:///casa");
                s.touched_ms = *tocado;
                b.slots.insert(id, s);
            }
            b.layouts.insert((*nombre).to_owned(), arbol);
        }
        b
    }

    /// Las MARCAS (fase 9) son aditivas: un cuerpo sin ellas se lee igual que
    /// antes, y uno con ellas las devuelve por RUTA.
    ///
    /// Aditivo de verdad quiere decir dos cosas, y las dos se comprueban: un
    /// documento viejo —que no tiene el campo— sigue leyéndose sin subir
    /// [`SCHEMA_VERSION`], y un hueco sin marcas produce los MISMOS bytes que
    /// producía antes de que el campo existiera. Sin lo segundo, cada tic
    /// escribiría un cuerpo distinto del anterior y el coalescing dejaría de
    /// coalescer.
    #[test]
    fn las_marcas_son_aditivas_y_viajan_por_ruta() {
        let mut s = slot("mem:///casa");
        assert_eq!(
            serde_json::to_value(&s).expect("json").get("marks"),
            None,
            "un hueco sin marcas no escribe el campo"
        );
        // Y se lee un documento que no lo trae, que es todo lo que había
        // guardado hasta esta versión.
        let viejo = serde_json::json!({"path": "mem:///casa"});
        let leido: SlotState = serde_json::from_value(viejo).expect("un cuerpo viejo se lee");
        assert!(leido.marks.is_empty());

        s.marks = vec![vp("mem:///casa/a.txt"), vp("mem:///casa/b.txt")];
        let ida = serde_json::to_value(&s).expect("json");
        let vuelta: SlotState = serde_json::from_value(ida).expect("json");
        assert_eq!(vuelta.marks, s.marks, "vuelven las RUTAS, no unos índices");
    }

    /// Pasado el tope, el estado del perfil que hace más que nadie activa se va
    /// ENTERO. Su directorio de configuración no se toca: el perfil sigue
    /// existiendo y arranca de su `[profile.start]`.
    #[test]
    fn pasado_el_tope_se_va_el_perfil_mas_viejo() {
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "e".to_owned();
        b.prune(100);
        assert!(!b.layouts.contains_key("a"), "el más viejo se va");
        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP);
    }

    /// ADR 0139: la disposición de la ventana (`<perfil>@window`) es del
    /// mismo perfil que la de la terminal: no cuenta como un perfil más, y
    /// se va y se queda con él.
    #[test]
    fn la_disposicion_de_la_ventana_va_con_su_perfil() {
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40)]);
        for p in ["a", "d"] {
            let arbol = b.layouts[p].clone();
            b.layouts.insert(window_layout_key(p), arbol);
        }
        b.active = "d".to_owned();
        b.prune(100);
        assert_eq!(
            b.layouts.len(),
            6,
            "cuatro perfiles, no seis: nada que podar"
        );
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        for p in ["a", "e"] {
            let arbol = b.layouts[p].clone();
            b.layouts.insert(window_layout_key(p), arbol);
        }
        b.active = "e".to_owned();
        b.prune(100);
        assert!(!b.layouts.contains_key("a"));
        assert!(!b.layouts.contains_key("a@window"), "se va con su perfil");
        assert!(b.layouts.contains_key("e@window"), "la del activo se queda");
    }

    /// El ACTIVO no lo barre nada, en ningún paso, ni siendo el más viejo.
    #[test]
    fn el_activo_no_se_barre_aunque_sea_el_mas_viejo() {
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "a".to_owned();
        b.prune(100);
        assert!(b.layouts.contains_key("a"), "el activo se queda");
        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP);
    }

    /// Y tirar un perfil se lleva SUS huecos, no los de otro.
    #[test]
    fn tirar_un_perfil_se_lleva_solo_sus_huecos() {
        let mut b = cuerpo_con_perfiles(&[("a", 10), ("b", 20), ("c", 30), ("d", 40), ("e", 50)]);
        b.active = "e".to_owned();
        let de_a: Vec<u32> = b.layouts["a"].slot_ids().iter().map(|s| s.0).collect();
        let de_b: Vec<u32> = b.layouts["b"].slot_ids().iter().map(|s| s.0).collect();
        b.prune(100);
        for id in de_a {
            assert!(
                !b.slots.contains_key(&id),
                "el hueco {id} de «a» se fue con él"
            );
        }
        for id in de_b {
            assert!(b.slots.contains_key(&id), "el hueco {id} de «b» sigue ahí");
        }
    }

    /// Un cuerpo que llega de DISCO puede compartir ids entre dos perfiles: la
    /// disjunción es un invariante del reparto, y `from_value` solo valida cada
    /// árbol por separado. Tirar un perfil no puede llevarse por delante los
    /// huecos del ACTIVO, que es lo que el rustdoc de `prune` promete.
    #[test]
    fn tirar_un_perfil_no_toca_huecos_que_otro_sigue_mencionando() {
        let compartido = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let mut b = SessionBody::default();
        // Cinco perfiles con LOS MISMOS ids: nada en el esquema lo prohíbe.
        for (i, nombre) in ["a", "b", "c", "d", "e"].iter().enumerate() {
            b.layouts.insert((*nombre).to_owned(), compartido.clone());
            for SlotId(id) in compartido.slot_ids() {
                let mut s = slot("file:///casa");
                s.touched_ms = (i as u64 + 1) * 10;
                b.slots.insert(id, s);
            }
        }
        b.active = "e".to_owned();
        b.prune(100);

        assert_eq!(b.layouts.len(), PROFILE_STATE_CAP, "sobra uno y se va");
        for SlotId(id) in b.layouts[&b.active].slot_ids() {
            assert!(
                b.slots.contains_key(&id),
                "el hueco {id} lo sigue enseñando el perfil activo"
            );
        }
    }

    /// Sin espacio arriba, `next_slot_base` lo DICE en vez de contestar un id
    /// que está en uso, y `rebase_slot_ids` devuelve el árbol intacto en vez de
    /// repartir el mismo número a todos sus huecos — lo que fabricaría
    /// duplicados a partir de un árbol sano y haría que el siguiente
    /// `from_value` rehusara el cuerpo ENTERO.
    #[test]
    fn sin_espacio_arriba_no_se_reparte_nada() {
        let mut b = SessionBody::default();
        b.slots.insert(u32::MAX, slot("file:///casa"));
        assert_eq!(b.next_slot_base(), None, "no hay «el siguiente»");

        let arbol = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let (nuevo, mapa) = arbol.rebase_slot_ids(u32::MAX);
        assert_eq!(nuevo, arbol, "el árbol vuelve tal cual");
        assert!(mapa.is_empty());
        assert!(
            nuevo.duplicate_slot_ids().is_empty(),
            "y sobre todo: sin duplicados fabricados"
        );
    }

    /// Un cuerpo realista con el tope lleno cabe en `SESSION_BODY_MAX`. Si este
    /// test se pone rojo, la cura es BAJAR [`PROFILE_STATE_CAP`], no subir el
    /// tope del protocolo: el core rehúsa un `put` que se pase y deja la sesión
    /// como estaba, así que pasarse es perder lo que estabas haciendo.
    /// La raíz de las rutas de los tests del sobre: de este mismo repositorio,
    /// porque una ruta rellenada a mano mediría el relleno y no el caso.
    const RAIZ: &str = "file:///home/u/src/norte/crates/norte-frontend/src";

    /// Un hueco con el historial lleno en los dos sentidos.
    fn slot_con_historial_lleno(id: u32) -> SlotState {
        let mut s = slot(&format!("{RAIZ}/modulo{id}"));
        s.back = (0..HISTORY_CAP)
            .map(|i| vp(&format!("{RAIZ}/modulo{id}/atras{i}")))
            .collect();
        s.forward = (0..HISTORY_CAP)
            .map(|i| vp(&format!("{RAIZ}/modulo{id}/alante{i}")))
            .collect();
        s
    }

    /// [`PROFILE_STATE_CAP`] perfiles de ocho huecos, todos con el historial
    /// lleno: el cuerpo VISIBLE al tope, sin un solo huérfano.
    fn cuerpo_visible_al_tope() -> SessionBody {
        let mut b = SessionBody::default();
        for p in 0..PROFILE_STATE_CAP {
            let hijos: Vec<Node> = (1..=8u32)
                .map(|i| Node::slot(SlotId(i), KindId::browser()))
                .collect();
            let arbol = crate::layout::Node::split(crate::layout::Dir::Horizontal, hijos);
            let (arbol, _) = arbol.rebase_slot_ids(b.next_slot_base().expect("hay sitio"));
            for SlotId(id) in arbol.slot_ids() {
                b.slots.insert(id, slot_con_historial_lleno(id));
            }
            b.layouts.insert(format!("perfil{p}"), arbol);
        }
        b.active = "perfil0".to_owned();
        b
    }

    #[test]
    fn un_cuerpo_realista_con_el_tope_lleno_cabe_en_el_sobre() {
        let mut b = cuerpo_visible_al_tope();
        b.prune(0);

        let bytes = serde_json::to_vec(&b.to_value()).expect("serializa");
        assert!(
            bytes.len() <= norte_proto::methods::SESSION_BODY_MAX,
            "{} bytes contra un tope de {}",
            bytes.len(),
            norte_proto::methods::SESSION_BODY_MAX
        );
    }

    /// **Y un cuerpo que se pasa CABIENDO en todas las cuentas, también cabe**
    /// al final: el tope de verdad es de bytes y ninguna cuenta puede
    /// prometerlo (revisión de #304).
    ///
    /// Aquí no hay ni un huérfano y los cuatro perfiles son los que la cuenta
    /// permite; lo que se pasa es el número de huecos VISIBLES, que no tiene
    /// tope ninguno — nada impide veinte pestañas por perfil. Sin la poda por
    /// bytes, el core rehusaba el `put` ENTERO.
    ///
    /// Lo que NO se puede perder está comprobado aparte: el perfil activo, su
    /// disposición y la RUTA de cada hueco suyo. Lo que se paga son pasos de
    /// historial, que es el orden declarado en `fit_to_envelope`.
    #[test]
    fn un_cuerpo_que_cabe_en_las_cuentas_y_no_en_los_bytes_se_recorta_igual() {
        let mut b = SessionBody::default();
        for p in 0..PROFILE_STATE_CAP {
            let hijos: Vec<Node> = (1..=40u32)
                .map(|i| Node::slot(SlotId(i), KindId::browser()))
                .collect();
            let arbol = crate::layout::Node::split(crate::layout::Dir::Horizontal, hijos);
            let (arbol, _) = arbol.rebase_slot_ids(b.next_slot_base().expect("hay sitio"));
            for SlotId(id) in arbol.slot_ids() {
                b.slots.insert(id, slot_con_historial_lleno(id));
            }
            b.layouts.insert(format!("perfil{p}"), arbol);
        }
        b.active = "perfil0".to_owned();
        let activo = b.layouts["perfil0"].clone();
        let rutas_del_activo: Vec<(u32, VPath)> = activo
            .slot_ids()
            .into_iter()
            .map(|SlotId(id)| (id, b.slots[&id].path.clone()))
            .collect();

        b.prune(0);

        let bytes = serde_json::to_vec(&b.to_value()).expect("serializa");
        assert!(
            bytes.len() <= norte_proto::methods::SESSION_BODY_MAX,
            "{} bytes contra un tope de {}",
            bytes.len(),
            norte_proto::methods::SESSION_BODY_MAX
        );
        assert_eq!(b.layouts.get("perfil0"), Some(&activo), "el activo entero");
        for (id, path) in rutas_del_activo {
            assert_eq!(
                b.slots.get(&id).map(|s| &s.path),
                Some(&path),
                "el hueco {id} del perfil activo conserva DÓNDE está"
            );
        }
    }

    /// Degradar tira el historial y NADA más: las rutas, el cursor y las
    /// disposiciones se quedan, que es lo que había que salvar (#316).
    ///
    /// Y dice si quedaba algo que tirar, porque reintentar sin haber degradado
    /// es pedir el mismo error otra vez.
    #[test]
    fn degradar_tira_el_historial_y_solo_el_historial() {
        let mut b = cuerpo_visible_al_tope();
        let antes = b.layouts.clone();
        let rutas: BTreeMap<u32, VPath> = b
            .slots
            .iter()
            .map(|(id, s)| (*id, s.path.clone()))
            .collect();

        assert!(b.degrade_for_size(), "había historial que tirar");
        assert!(
            b.slots
                .values()
                .all(|s| s.back.is_empty() && s.forward.is_empty()),
            "no queda un solo paso"
        );
        assert_eq!(b.layouts, antes, "las disposiciones no se tocan");
        for (id, path) in rutas {
            assert_eq!(b.slots[&id].path, path, "el hueco {id} sigue donde estaba");
        }
        assert!(
            !b.degrade_for_size(),
            "y a la segunda no queda nada: reintentar sería el mismo error"
        );
    }

    /// Y el tope de HUÉRFANOS lleno también cabe, que es lo que no pasaba
    /// (#304): [`ORPHAN_CAP`] huecos que nadie mira, cada uno con el historial
    /// lleno, sobre el cuerpo visible al tope. Con [`HISTORY_CAP`] para todos
    /// esto daba ~1 182 000 bytes contra los 1 048 576 del sobre, y el `put`
    /// se rehusaba ENTERO: el recorte que existía para impedirlo lo causaba.
    #[test]
    fn el_tope_de_huerfanos_lleno_tambien_cabe_en_el_sobre() {
        let mut b = cuerpo_visible_al_tope();
        let base = b.next_slot_base().expect("hay sitio");
        for k in 0..u32::try_from(ORPHAN_CAP).expect("cabe") {
            let id = base + k;
            b.slots.insert(id, slot_con_historial_lleno(id));
        }
        b.prune(0);

        assert_eq!(
            b.slots.len(),
            8 * PROFILE_STATE_CAP + ORPHAN_CAP,
            "ni uno se ha barrido: el tope se llena, no se pasa"
        );
        let bytes = serde_json::to_vec(&b.to_value()).expect("serializa");
        assert!(
            bytes.len() <= norte_proto::methods::SESSION_BODY_MAX,
            "{} bytes contra un tope de {}",
            bytes.len(),
            norte_proto::methods::SESSION_BODY_MAX
        );
    }

    /// La base sale de TODO lo que hay: las disposiciones de cada perfil y los
    /// huecos guardados, huérfanos incluidos. Mirar solo el perfil activo
    /// reasignaría encima del estado de otro.
    #[test]
    fn la_base_deja_atras_todo_lo_que_ya_existe() {
        let mut b = SessionBody::default();
        b.layouts
            .insert("work".into(), Node::slot(SlotId(4), KindId::browser()));
        b.slots.insert(9, slot("file:///tmp"));
        assert_eq!(b.next_slot_base(), Some(10));
    }

    #[test]
    fn una_sesion_vacia_empieza_en_uno() {
        assert_eq!(SessionBody::default().next_slot_base(), Some(1));
    }

    /// Dos perfiles adoptados sobre la MISMA disposición de fábrica acaban con
    /// conjuntos de huecos disjuntos. Éste es el test que fija el diseño.
    #[test]
    fn dos_perfiles_sobre_la_misma_disposicion_no_comparten_hueco() {
        let fabrica = crate::layout::Node::split(
            crate::layout::Dir::Horizontal,
            vec![
                Node::slot(SlotId(1), KindId::browser()),
                Node::slot(SlotId(2), KindId::browser()),
            ],
        );
        let mut b = SessionBody::default();

        let (t1, _) = fabrica.rebase_slot_ids(b.next_slot_base().expect("hay sitio"));
        b.layouts.insert("work".into(), t1);
        let (t2, _) = fabrica.rebase_slot_ids(b.next_slot_base().expect("hay sitio"));
        b.layouts.insert("photos".into(), t2);

        let a: BTreeSet<SlotId> = b.layouts["work"].slot_ids().into_iter().collect();
        let c: BTreeSet<SlotId> = b.layouts["photos"].slot_ids().into_iter().collect();
        assert!(a.is_disjoint(&c), "work {a:?} y photos {c:?} se pisan");
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
            active: String::new(),
            layouts: std::iter::once(("default".to_owned(), sin_listado)).collect(),
            slots: std::collections::BTreeMap::new(),
            palette_recent: Vec::new(),
            popular: Vec::new(),
        };
        let v = body.to_value();
        assert!(matches!(
            SessionBody::from_value(SCHEMA_VERSION, &v),
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
        let vuelta = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("parsea");
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
            let vuelta = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("parsea");
            assert_eq!(
                vuelta.slots[&1].path.file_name().map(Segment::as_bytes),
                ruta.file_name().map(Segment::as_bytes),
                "{} no sobrevivió",
                name.id
            );
            assert_eq!(vuelta.slots[&1].path, ruta, "{}", name.id);
        }
    }

    /// Un cuerpo v1 no trae `active`, y eso significa exactamente «sin
    /// perfil». Leerlo tiene que seguir funcionando: quitarle la sesión a
    /// quien actualiza el binario es justo lo que ADR 0059 promete que no
    /// pasa.
    #[test]
    fn un_cuerpo_v1_se_lee_como_sin_perfil() {
        let v1 = serde_json::json!({ "version": 1, "layouts": {}, "slots": {} });
        let b = SessionBody::from_value(1, &v1).expect("un v1 se sigue leyendo");
        assert_eq!(b.active, "", "sin perfil, que es la verdad");
    }

    #[test]
    fn el_perfil_activo_sobrevive_al_viaje() {
        let mut b = SessionBody {
            active: "work".to_owned(),
            ..SessionBody::default()
        };
        b.layouts
            .insert("work".into(), Node::slot(SlotId(1), KindId::browser()));
        let vuelta = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("ida y vuelta");
        assert_eq!(vuelta.active, "work");
    }

    /// Spec 2026-09-15 D5/D6: el punto de salto y los populares hacen el viaje,
    /// y un cuerpo escrito antes de que existieran se sigue leyendo sin ellos.
    #[test]
    fn el_punto_de_salto_y_los_populares_hacen_el_viaje() {
        let mut b = SessionBody::default();
        b.slots.insert(
            1,
            SlotState {
                jump: Some(vp("file:///marcado")),
                ..slot("file:///casa")
            },
        );
        b.popular.push(crate::history::PopularEntry {
            path: vp("file:///frecuente"),
            visits: 3,
            last: 9,
        });
        let vuelta = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("ida y vuelta");
        assert_eq!(vuelta.slots[&1].jump, Some(vp("file:///marcado")));
        assert_eq!(vuelta.popular, b.popular);

        let viejo = serde_json::json!({
            "layouts": {},
            "slots": { "1": { "path": "file:///casa" } },
        });
        let b = SessionBody::from_value(SCHEMA_VERSION, &viejo).expect("un cuerpo viejo carga");
        assert_eq!(b.slots[&1].jump, None);
        assert!(b.popular.is_empty());
    }

    #[test]
    fn la_poda_deja_los_populares_en_su_tope_por_importancia() {
        let mut b = SessionBody {
            popular: (0..crate::history::POPULAR_CAP + 5)
                .map(|i| crate::history::PopularEntry {
                    path: vp(&format!("file:///d{i}")),
                    // Los cinco primeros son los MENOS visitados: los que se van.
                    visits: if i < 5 { 1 } else { 2 },
                    last: u64::try_from(i).expect("cabe"),
                })
                .collect(),
            ..SessionBody::default()
        };
        b.prune(0);
        assert_eq!(b.popular.len(), crate::history::POPULAR_CAP);
        assert!(b.popular.iter().all(|e| e.visits == 2));
    }

    /// Una entrada de populares ilegible se salta: no se lleva la sesión
    /// entera por delante (encoding-auditor, fase 1).
    #[test]
    fn un_popular_ilegible_se_salta_y_el_cuerpo_carga() {
        let v = serde_json::json!({
            "layouts": {},
            "slots": { "1": { "path": "file:///casa" } },
            "popular": [
                { "path": "no es una ruta", "visits": 9 },
                { "path": "file:///bien", "visits": 2, "last": 1 },
            ],
        });
        let b = SessionBody::from_value(SCHEMA_VERSION, &v).expect("el cuerpo carga");
        assert_eq!(
            b.slots[&1].path,
            vp("file:///casa"),
            "los huecos siguen ahí"
        );
        assert_eq!(b.popular.len(), 1);
        assert_eq!(b.popular[0].path, vp("file:///bien"));
    }

    /// Y un cuerpo del FUTURO se sigue rehusando entero: subir a 2 no puede
    /// abrir la puerta a un 3.
    #[test]
    fn un_cuerpo_v3_se_sigue_rehusando() {
        let v3 = serde_json::json!({ "layouts": {}, "slots": {}, "active": "x" });
        assert!(matches!(
            SessionBody::from_value(SCHEMA_VERSION + 1, &v3),
            Err(SessionError::FromTheFuture { .. })
        ));
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
        let vuelta = SessionBody::from_value(SCHEMA_VERSION, &b.to_value()).expect("parsea");
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
        b.layouts
            .insert("default".into(), Node::slot(SlotId(1), KindId::browser()));
        b.prune(0);
        let back = &b.slots[&1].back;
        assert_eq!(back.len(), HISTORY_CAP);
        assert_eq!(
            back.last().expect("último"),
            &vp(&format!("file:///d{}", HISTORY_CAP + 9))
        );
    }

    /// Y el de un HUÉRFANO se recorta más (#304): nadie puede pulsar «atrás»
    /// dentro de un hueco que ninguna disposición menciona sin volver a abrirlo
    /// antes. Se tira por el mismo extremo, el viejo.
    #[test]
    fn el_historial_de_un_huerfano_se_recorta_mas() {
        let mut b = SessionBody::default();
        let mut s = slot("file:///casa");
        s.back = (0..HISTORY_CAP)
            .map(|i| vp(&format!("file:///d{i}")))
            .collect();
        s.forward = s.back.clone();
        b.slots.insert(1, s);
        b.layouts
            .insert("default".into(), Node::slot(SlotId(2), KindId::browser()));
        b.prune(0);
        let s = &b.slots[&1];
        assert_eq!(s.back.len(), ORPHAN_HISTORY_CAP);
        assert_eq!(s.forward.len(), ORPHAN_HISTORY_CAP);
        assert_eq!(
            s.back.last().expect("último"),
            &vp(&format!("file:///d{}", HISTORY_CAP - 1)),
            "lo reciente se queda"
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
        let b = SessionBody::from_value(SCHEMA_VERSION, &v).expect("parsea");
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
            SessionBody::from_value(SCHEMA_VERSION, &v),
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
        let b = SessionBody::from_value(SCHEMA_VERSION, &v).expect("parsea");
        assert_eq!(b.slots[&1].columns, vec!["name".parse().expect("name")]);
        assert_eq!(b.slots[&1].path, vp("file:///casa"));
    }
}
