//! El gestor de extensiones (`app.extensions`) visto desde el host: qué hay
//! instalado, en qué estado, y qué se le ha configurado.
//!
//! **Solo lectura, y no por comodidad.** Aprobar las capabilities de un
//! plugin es la decisión de seguridad del sistema de extensiones: es lo que
//! separa «este código está en tu disco» de «este código puede leer tus
//! ficheros». Esta ventana la ENSEÑA y no la toma, igual que no borra, hasta
//! que la fase 5 traiga el camino seguro. Por eso el `HostBackend` de este
//! host no tiene `set_approval` ni `set_enabled`: lo que no está no se puede
//! llamar por accidente.
//!
//! Todo lo que un plugin escribe —su nombre, su publicador, su versión, su
//! descripción, la descripción de cada clave, el VALOR de cada clave, su
//! defecto y los valores de un `enum`— es texto de TERCERO y se enmascara en
//! la ENTRADA, que es este módulo. Lo único que no lo es son la CLAVE (charset
//! validado por el manifiesto) y el TIPO (conjunto cerrado); las cotas son
//! números. Esa lista decía antes que el valor, el defecto y el dominio eran
//! seguros: no lo son — el manifiesto les acota la longitud y nada más.

use norte_frontend::help_badge::{plugin_description, plugin_label};
use norte_frontend::plugin_config::sanitize_config_keys;
use norte_i18n::Lang;
use norte_proto::methods::{PluginGetConfigResult, PluginListResult};

use crate::bridge::clamp_display;
use crate::dto::{
    ExtensionConfigRowView, ExtensionDetailView, ExtensionErrorView, ExtensionRowView,
    ExtensionsView,
};

/// Tope de filas del catálogo que cruzan.
///
/// Un daemon hostil puede anunciar los plugins que quiera, y cada fila cuesta
/// varias cadenas enmascaradas. El tope acota el trabajo y el mensaje; lo que
/// se deja fuera NO se calla, se dice en la propia vista.
const MAX_EXTENSIONES: usize = 512;

/// El gestor abierto.
pub(crate) struct Extensiones {
    /// Lo instalado, ya saneado. Vacío mientras el catálogo no llega.
    filas: Vec<ExtensionRowView>,
    /// Los directorios que no cargaron, ya saneados.
    errores: Vec<ExtensionErrorView>,
    /// Cuál está elegida.
    cursor: usize,
    /// El catálogo todavía no ha contestado.
    cargando: bool,
    /// La ficha abierta, si alguna.
    ficha: Option<ExtensionDetailView>,
    /// La extensión cuya ficha se ha PEDIDO. Se guarda para descartar una
    /// respuesta que llega cuando el lector ya se fue a otra fila: sin esto,
    /// una ficha lenta aterrizaba encima de otra extensión.
    pedida: Option<String>,
}

impl Extensiones {
    /// Abre el gestor vacío: el catálogo se pide y llega después.
    pub(crate) fn abrir() -> Self {
        Self {
            filas: Vec::new(),
            errores: Vec::new(),
            cursor: 0,
            cargando: true,
            ficha: None,
            pedida: None,
        }
    }

    /// Mete el catálogo que contestó el daemon.
    pub(crate) fn set_catalogo(&mut self, lista: &PluginListResult) {
        self.cargando = false;
        self.filas = lista
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(MAX_EXTENSIONES)
            .map(fila_de)
            .collect();
        self.errores = lista
            .errors
            .iter()
            .take(MAX_EXTENSIONES)
            .map(|e| {
                let (dir, enmascarado) = norte_frontend::display_name(e.dir.as_bytes());
                let (reason, reason_enmascarado) =
                    norte_frontend::display_name(e.reason.as_bytes());
                ExtensionErrorView {
                    dir: clamp_display(dir),
                    // O el U+FFFD ya estaba. El daemon manda `dir` como
                    // `String` y lo produce con un `to_string_lossy` SIN
                    // marcar, así que un directorio llamado `caf\xff` llega
                    // aquí ya convertido: `display_name` no vuelve a
                    // marcarlo —U+FFFD no es un peligro de terminal, es
                    // Specials— y la fila decía ser fiel. Ver la trampa del
                    // doble lossy: la bandera no se recupera, pero el
                    // REEMPLAZO sí se ve, y verlo ya significa que lo
                    // pintado difiere de lo que hay.
                    //
                    // Es media solución: `lossy_collapse_ff` y
                    // `lossy_collapse_fe` siguen colapsando en la misma fila,
                    // y distinguirlas pide que el daemon mande los bytes o su
                    // marca (issue abierta). Marcarlas es lo que se puede
                    // hacer desde este lado, y es estrictamente mejor que no
                    // marcarlas.
                    hostile: enmascarado || dir_ya_convertido(&e.dir),
                    // El motivo lo escribe el core, pero puede CITAR el
                    // manifiesto del plugin —y un `Path::display()`—, así que
                    // entra por la misma puerta que el resto del texto de
                    // tercero.
                    reason: clamp_display(reason),
                    reason_hostile: reason_enmascarado || dir_ya_convertido(&e.reason),
                }
            })
            .collect();
        self.cursor = self.cursor.min(self.filas.len().saturating_sub(1));
    }

    /// La extensión elegida, si hay alguna.
    pub(crate) fn elegida(&self) -> Option<&str> {
        self.filas.get(self.cursor).map(|f| f.id.as_str())
    }

    /// Mueve el cursor y TIRA la ficha: describe otra extensión.
    pub(crate) fn mover(&mut self, delta: i64) {
        if self.filas.is_empty() {
            return;
        }
        let destino = i64::try_from(self.cursor)
            .unwrap_or(0)
            .saturating_add(delta);
        let nuevo = usize::try_from(destino.max(0))
            .unwrap_or(0)
            .min(self.filas.len() - 1);
        if nuevo != self.cursor {
            self.cursor = nuevo;
            self.cerrar_ficha();
        }
    }

    /// Pone el cursor en una fila concreta (un click). Fuera de rango no hace
    /// nada: quien pinta puede ir un frame por detrás.
    pub(crate) fn senalar(&mut self, fila: usize) {
        if fila < self.filas.len() && fila != self.cursor {
            self.cursor = fila;
            self.cerrar_ficha();
        }
    }

    /// Cierra la ficha y olvida lo pedido.
    pub(crate) fn cerrar_ficha(&mut self) {
        self.ficha = None;
        self.pedida = None;
    }

    /// `true` si hay una ficha abierta que cerrar.
    pub(crate) fn tiene_ficha(&self) -> bool {
        self.ficha.is_some()
    }

    /// Reclama la ficha de la extensión elegida, si no está ya pedida.
    pub(crate) fn reclamar_ficha(&mut self) -> Option<String> {
        let id = self.elegida()?.to_owned();
        if self.pedida.as_deref() == Some(id.as_str()) {
            return None;
        }
        self.pedida = Some(id.clone());
        Some(id)
    }

    /// Instala la ficha que contestó el daemon.
    ///
    /// Se descarta si el lector ya se fue a otra fila: una respuesta lenta no
    /// puede describir una extensión distinta de la que está señalada.
    pub(crate) fn set_ficha(&mut self, id: &str, res: &PluginGetConfigResult, lang: Lang) {
        if self.pedida.as_deref() != Some(id) {
            return;
        }
        self.ficha = Some(ExtensionDetailView {
            id: id.to_owned(),
            config: sanitize_config_keys(&res.keys)
                .into_iter()
                .map(|k| {
                    let domain = dominio(&k, lang);
                    ExtensionConfigRowView {
                        key: clamp_display(k.key),
                        kind: clamp_display(k.kind),
                        // Del lado de PINTAR, nunca del operando: `k.value`
                        // es lo que un editor escribiría de vuelta.
                        value: clamp_display(k.display.value.clone()),
                        default: clamp_display(k.display.default.clone()),
                        description: clamp_display(k.description),
                        domain: clamp_display(domain),
                        hostile: k.display.hostile,
                    }
                })
                .collect(),
        });
    }

    /// La proyección.
    pub(crate) fn vista(&self) -> ExtensionsView {
        ExtensionsView {
            rows: self.filas.clone(),
            cursor: self.cursor as u64,
            detail: self.ficha.clone(),
            loading: self.cargando,
            errors: self.errores.clone(),
        }
    }
}

/// Qué acota una clave: los valores de un `enum`, las cotas de un `int`, o
/// nada.
fn dominio(k: &norte_frontend::plugin_config::ConfigKeyRow, lang: Lang) -> String {
    if !k.display.values.is_empty() {
        // Los ENMASCARADOS: un valor de `enum` es texto que escribe el
        // plugin, y este `·` es una composición en banda.
        return k.display.values.join(" · ");
    }
    match (k.min, k.max) {
        (Some(min), Some(max)) => norte_i18n::ta_in(
            lang,
            "ext-config-range",
            &[("min", &min.to_string()), ("max", &max.to_string())],
        ),
        (Some(min), None) => {
            norte_i18n::ta_in(lang, "ext-config-min", &[("min", &min.to_string())])
        }
        (None, Some(max)) => {
            norte_i18n::ta_in(lang, "ext-config-max", &[("max", &max.to_string())])
        }
        (None, None) => String::new(),
    }
}

/// Una fila del catálogo, saneada.
fn fila_de(p: &norte_proto::methods::PluginInfo) -> ExtensionRowView {
    let nombre = plugin_label(&p.name);
    ExtensionRowView {
        // El id NO se enmascara y NO se recorta: es una clave validada
        // (reverse-DNS), y las dos cosas la romperían — enmascarar no es
        // inyectivo y recortar tampoco.
        id: p.id.clone(),
        name: clamp_display(if norte_help::is_blank_id(&nombre) {
            // Un `name` de rellenos invisibles es un manifiesto legal cuya
            // fila se pinta en blanco: se cae al id, que es lo único que el
            // host asigna.
            plugin_label(&p.id)
        } else {
            nombre
        }),
        publisher: clamp_display(plugin_label(&p.publisher)),
        version: clamp_display(plugin_label(&p.version)),
        // La categoría es vocabulario del CORE (`previewer`, `indexer`…), no
        // texto libre del manifiesto: aun así entra por la misma puerta,
        // porque quien la manda es el daemon y no este proceso.
        category: clamp_display(plugin_label(&p.category)),
        description: clamp_display(
            p.description
                .as_deref()
                .map(plugin_description)
                .unwrap_or_default(),
        ),
        approved: p.approved,
        enabled: p.enabled,
        has_help: p.has_help,
        commands: u32::try_from(p.commands.len()).unwrap_or(u32::MAX),
        columns: u32::try_from(p.columns.len()).unwrap_or(u32::MAX),
        capabilities: p
            .capabilities
            .iter()
            .map(|c| clamp_display(plugin_label(c)))
            .collect(),
    }
}

/// La cadena ya trae el reemplazo de una conversión con pérdida que hizo
/// OTRO.
///
/// norte no escribe U+FFFD nunca salvo como máscara, así que encontrarlo en
/// algo que todavía no ha pasado por la máscara significa que alguien aguas
/// arriba convirtió bytes que no eran UTF-8 y no lo dijo.
fn dir_ya_convertido(s: &str) -> bool {
    s.contains('\u{fffd}')
}
