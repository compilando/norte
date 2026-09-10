//! El gestor de extensiones (`app.extensions`) visto desde el host: qué hay
//! instalado, en qué estado, y qué se le ha configurado.
//!
//! **Ya gobierna, y con el freno puesto.** Aprobar las capabilities de un
//! plugin es la decisión de seguridad del sistema de extensiones: es lo que
//! separa «este código está en tu disco» de «este código puede leer tus
//! ficheros». Desde la 6.4 esta ventana la toma, con tres cosas que no son
//! adorno:
//!
//! - **Aprobar PREGUNTA**, y la pregunta enumera las capabilities una por
//!   línea. Revocar y apagar no preguntan: van en la dirección segura.
//! - **Nada de esto existe en `SoloLectura`.** El mismo interruptor que
//!   decide si la ventana borra decide si concede permisos.
//! - **La verdad vive en el core.** Tras un cambio se REPIDE el catálogo en
//!   vez de tocar el `bool` de aquí: un optimismo local que el daemon no
//!   confirmó es una pantalla que miente sobre quién puede leer tus ficheros.
//!
//! Todo lo que un plugin escribe —su nombre, su publicador, su versión, su
//! descripción, la descripción de cada clave, el VALOR de cada clave, su
//! defecto y los valores de un `enum`— es texto de TERCERO y se enmascara en
//! la ENTRADA, que es este módulo. Lo único que no lo es son la CLAVE (charset
//! validado por el manifiesto) y el TIPO (conjunto cerrado); las cotas son
//! números. Esa lista decía antes que el valor, el defecto y el dominio eran
//! seguros: no lo son — el manifiesto les acota la longitud y nada más.

use norte_frontend::help_badge::{plugin_description, plugin_label, plugin_label_flagged};
use norte_frontend::plugin_config::{PendingConfigWrite, PluginConfigState, sanitize_config_keys};
use norte_frontend::settings::SettingsEditError;
use norte_i18n::Lang;
use norte_proto::methods::{PluginGetConfigResult, PluginInfo, PluginListResult};

use crate::bridge::clamp_display;
use crate::dto::{
    ExtensionCommandView, ExtensionConfigRowView, ExtensionDetailView, ExtensionErrorView,
    ExtensionRowView, ExtensionsView,
};

/// Tope de filas del catálogo que cruzan.
///
/// Un daemon hostil puede anunciar los plugins que quiera, y cada fila cuesta
/// varias cadenas enmascaradas. El tope acota el trabajo y el mensaje; lo que
/// se deja fuera NO se calla, se dice en la propia vista.
pub(crate) const MAX_EXTENSIONES: usize = 512;

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
    ficha: Option<Ficha>,
    /// El catálogo CRUDO tal como llegó, ya filtrado y acotado.
    ///
    /// Las filas de arriba son su proyección enmascarada, y de una máscara no
    /// se vuelve: para preguntar «¿apruebas ESTAS capabilities?» hace falta
    /// enmascarar cada una POR SEPARADO y saber cuál difiere, y eso solo se
    /// puede hacer desde el texto original.
    catalogo: Vec<PluginInfo>,
    /// Los comandos de cada extensión, ya enmascarados, por id de extensión.
    ///
    /// Aparte de la fila y no dentro: la fila cruza el puente en CADA
    /// repintado del catálogo, y los títulos de los comandos solo hacen falta
    /// cuando alguien abre una ficha. El enmascarado se hace una vez, aquí.
    comandos: std::collections::HashMap<String, Vec<ExtensionCommandView>>,
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
            catalogo: Vec::new(),
            comandos: std::collections::HashMap::new(),
            pedida: None,
        }
    }

    /// Mete el catálogo que contestó el daemon.
    pub(crate) fn set_catalogo(&mut self, lista: &PluginListResult) {
        self.cargando = false;
        // Quién estaba elegida, por ID. Un cambio de estado REPIDE el
        // catálogo entero, y el core lo ordena por categoría e id: aprobar
        // una extensión puede moverla de sitio, y un cursor por posición
        // dejaría al lector señalando otra distinta justo después de haberle
        // concedido permisos a la primera.
        let elegida = self.elegida().map(str::to_owned);
        self.catalogo = lista
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(MAX_EXTENSIONES)
            .cloned()
            .collect();
        self.comandos = lista
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(MAX_EXTENSIONES)
            .map(|p| (p.id.clone(), comandos_de(p)))
            .collect();
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
                // Los BYTES si el peer los manda (#265), y solo entonces
                // `display_name` puede hacer la conversión y MARCARLA. La
                // cadena `dir` es el respaldo para un peer 0.52, que es donde
                // sigue valiendo la heurística de abajo.
                let (dir, enmascarado) = norte_frontend::display_name(
                    e.dir_bytes.as_deref().unwrap_or(e.dir.as_bytes()),
                );
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
                    // Era media solución: `lossy_collapse_ff` y
                    // `lossy_collapse_fe` colapsaban en la misma fila, y
                    // distinguirlas pedía los bytes. Desde 0.53.0 el daemon
                    // los manda (#265) y esta rama es solo el respaldo para
                    // un peer viejo.
                    // Con bytes, la bandera de `display_name` es la buena y
                    // la heurística sobra —y sería un falso positivo sobre un
                    // directorio que se llame `caf\u{FFFD}` de verdad—. Sin
                    // ellos, sigue siendo lo único que hay.
                    hostile: enmascarado || (e.dir_bytes.is_none() && dir_ya_convertido(&e.dir)),
                    // El motivo lo escribe el core, pero puede CITAR el
                    // manifiesto del plugin —y un `Path::display()`—, así que
                    // entra por la misma puerta que el resto del texto de
                    // tercero.
                    reason: clamp_display(reason),
                    reason_hostile: reason_enmascarado || dir_ya_convertido(&e.reason),
                }
            })
            .collect();
        if let Some(i) = elegida.and_then(|id| self.filas.iter().position(|f| f.id == id)) {
            self.cursor = i;
        } else {
            // La que estaba elegida ya no está —o el catálogo llegó
            // vacío, que es lo que pasa cuando la petición vence—: el
            // cursor cae donde puede y la FICHA se cierra. Sin esto, el
            // detalle seguía describiendo a una extensión mientras el
            // cursor señalaba a otra, y la siguiente tecla se aplicaba a
            // la señalada.
            self.cursor = self.cursor.min(self.filas.len().saturating_sub(1));
            self.cerrar_ficha();
        }
    }

    /// La fila elegida entera: lo que hace falta para gobernarla.
    pub(crate) fn fila_elegida(&self) -> Option<&ExtensionRowView> {
        self.filas.get(self.cursor)
    }

    /// El catálogo crudo, para dárselo a la ayuda: sus páginas de extensión
    /// salen de la misma lista que estas filas.
    pub(crate) fn catalogo(&self) -> &[PluginInfo] {
        &self.catalogo
    }

    /// Las filas tal como viajaron, para comprobar que un clic nombra una.
    pub(crate) fn filas(&self) -> &[ExtensionRowView] {
        &self.filas
    }

    /// Lo que hay que ENSEÑAR antes de conceder capabilities: el nombre de
    /// la extensión y sus capabilities, cada una enmascarada por su cuenta y
    /// con su propia bandera.
    ///
    /// Una bandera para todo el bloque no sirve aquí: quien lee tiene que
    /// saber CUÁL de las líneas se pinta distinta de lo que dice, y esa
    /// línea es justo la que un manifiesto hostil escribe para que parezca
    /// otra capability.
    pub(crate) fn concesion(&self, id: &str) -> Option<Concesion> {
        let p = self.catalogo.iter().find(|p| p.id == id)?;
        Some(Concesion {
            nombre: texto_de_tercero(&p.name),
            capabilities: p.capabilities.iter().map(|c| texto_de_tercero(c)).collect(),
            digest: p.manifest_digest.clone(),
        })
    }

    /// `true` si la ficha abierta es la de esta extensión.
    pub(crate) fn es_ficha_de(&self, id: &str) -> bool {
        self.ficha.as_ref().is_some_and(|f| f.id == id)
    }

    /// Los comandos de una extensión, ya enmascarados.
    pub(crate) fn comandos_de_id(&self, id: &str) -> &[ExtensionCommandView] {
        self.comandos.get(id).map_or(&[], Vec::as_slice)
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
        // El EDITOR compartido es el modelo, no una lista proyectada: es
        // quien sabe que un `bool` cicla, que un `int` se teclea y valida
        // contra sus cotas, y que un `kind` que este build no conoce —un
        // peer más nuevo— es de solo lectura en vez de un pánico.
        self.ficha = Some(Ficha {
            id: id.to_owned(),
            estado: PluginConfigState::new(sanitize_config_keys(&res.keys)),
            comandos: self.comandos.get(id).cloned().unwrap_or_default(),
            lang,
        });
    }

    /// Mueve el cursor DENTRO de la ficha. `false` si no hay ficha —o si la
    /// que hay no tiene claves que recorrer, y entonces las flechas son del
    /// catálogo: una ficha sin nada que andar que se quedara las teclas
    /// dejaría al lector sin poder moverse sin cerrarla primero.
    pub(crate) fn mover_en_ficha(&mut self, delta: i64) -> bool {
        let Some(f) = self.ficha.as_mut() else {
            return false;
        };
        if f.estado.rows().is_empty() {
            return false;
        }
        // El modelo compartido mueve de uno en uno y clampa; una página son N
        // pasos suyos, no un índice calculado aquí — que es cómo se acaba
        // teniendo dos respuestas a dónde está el cursor.
        let pasos = delta.unsigned_abs().min(f.estado.rows().len() as u64);
        for _ in 0..pasos {
            if delta < 0 {
                f.estado.up();
            } else {
                f.estado.down();
            }
        }
        true
    }

    /// `Enter` sobre la clave elegida: cicla un `bool`/`enum` —y entonces hay
    /// algo que escribir— o abre el buffer de edición de un `string`/`int`.
    ///
    /// Un `kind` desconocido no hace nada, que es la respuesta del modelo
    /// compartido: editar a ciegas una forma que este build no entiende es
    /// escribir en el `config.toml` de un plugin lo que a nadie le consta.
    pub(crate) fn activar_clave(&mut self) -> Option<(String, PendingConfigWrite)> {
        let f = self.ficha.as_mut()?;
        let write = f.estado.activate()?;
        Some((f.id.clone(), write))
    }

    /// `true` si el buffer de edición de la ficha está abierto.
    pub(crate) fn editando(&self) -> bool {
        self.ficha.as_ref().is_some_and(|f| f.estado.is_editing())
    }

    /// Un carácter al buffer de edición.
    pub(crate) fn escribir(&mut self, c: char) {
        if let Some(f) = self.ficha.as_mut() {
            f.estado.edit_push_char(c);
        }
    }

    /// Borra el último carácter del buffer.
    pub(crate) fn borrar(&mut self) {
        if let Some(f) = self.ficha.as_mut() {
            f.estado.edit_backspace();
        }
    }

    /// Cierra el buffer SIN escribir.
    pub(crate) fn cancelar_edicion(&mut self) {
        if let Some(f) = self.ficha.as_mut() {
            f.estado.edit_cancel();
        }
    }

    /// Confirma el buffer: el valor a escribir, o por qué no vale.
    ///
    /// # Errors
    /// Lo que diga el modelo compartido: no parsea como entero, o parsea y se
    /// sale de las cotas del esquema.
    pub(crate) fn confirmar_edicion(
        &mut self,
    ) -> Option<Result<(String, PendingConfigWrite), SettingsEditError>> {
        let f = self.ficha.as_mut()?;
        let id = f.id.clone();
        Some(f.estado.edit_commit().map(|w| (id, w)))
    }

    /// La proyección.
    pub(crate) fn vista(&self) -> ExtensionsView {
        ExtensionsView {
            rows: self.filas.clone(),
            cursor: self.cursor as u64,
            detail: self.ficha.as_ref().map(Ficha::vista),
            loading: self.cargando,
            errors: self.errores.clone(),
        }
    }
}

/// Una cadena de tercero lista para pintar y si difiere de lo que dice.
pub(crate) type Texto = (String, bool);

/// Lo que hay que ENSEÑAR antes de conceder capabilities.
pub(crate) struct Concesion {
    /// De quién son.
    pub(crate) nombre: Texto,
    /// Qué se concede, una por línea.
    pub(crate) capabilities: Vec<Texto>,
    /// El ancla del manifiesto que se ENSEÑÓ (#282), si el peer la manda.
    ///
    /// La comparación de capabilities que hace `conceder` cubre lo que se
    /// PINTA; ésta cubre lo que se CONCEDE, que es más: `category` y
    /// `contributions` —cuándo y cómo se dispara la extensión— entran en el
    /// ancla y no en la lista. Y la comparación local solo ve cambios dentro
    /// de este cliente: el `plugin.toml` que cambia bajo el core lo caza el
    /// core, con esto.
    pub(crate) digest: Option<String>,
}

/// La ficha abierta: QUIÉN, su editor de `[config]` y qué comandos aporta.
///
/// El editor es `norte_frontend::plugin_config::PluginConfigState`, el mismo
/// que mueve el gestor del TUI. Aquí no se decide qué cicla ni qué se teclea:
/// eso tendría dos respuestas en cuanto una de las dos superficies cambiara.
struct Ficha {
    /// De quién es la ficha.
    id: String,
    /// El editor compartido sobre sus claves.
    estado: PluginConfigState,
    /// Sus comandos, ya enmascarados.
    comandos: Vec<ExtensionCommandView>,
    /// Con qué idioma se compuso el dominio de cada clave.
    lang: Lang,
}

impl Ficha {
    /// La proyección de la ficha.
    fn vista(&self) -> ExtensionDetailView {
        ExtensionDetailView {
            id: self.id.clone(),
            config: self
                .estado
                .rows()
                .iter()
                .map(|k| {
                    let domain = dominio(k, self.lang);
                    ExtensionConfigRowView {
                        key: clamp_display(k.key.clone()),
                        kind: clamp_display(k.kind.clone()),
                        // Del lado de PINTAR, nunca del operando: `k.value`
                        // es lo que un editor escribiría de vuelta.
                        value: clamp_display(k.display.value.clone()),
                        default: clamp_display(k.display.default.clone()),
                        description: clamp_display(k.description.clone()),
                        domain: clamp_display(domain),
                        hostile: k.display.hostile,
                        // Lo dice el MISMO conjunto cerrado que el modelo
                        // compartido sabe editar. Sin esto, la pantalla
                        // ofrece `Enter` sobre una clave que no va a cambiar
                        // y el lector concluye que la escritura falló.
                        editable: k.is_editable(),
                    }
                })
                .collect(),
            commands: self.comandos.clone(),
            cursor: self.estado.cursor() as u64,
            editing: self.estado.edit_buffer().map(|b| {
                // El buffer se pinta SANEADO —lo teclea un humano, pero el
                // valor de partida lo escribió el plugin— y el operando
                // sigue crudo dentro del modelo compartido.
                clamp_display(norte_frontend::display_name(b.as_bytes()).0)
            }),
            editing_hostile: self
                .estado
                .edit_buffer()
                .is_some_and(|b| norte_frontend::display_name(b.as_bytes()).1),
        }
    }
}

/// Los comandos de una extensión, ya enmascarados.
///
/// El `id` de un comando NO se enmascara y NO se recorta: es la clave de
/// despacho que vuelve al daemon, y el manifiesto no le valida charset — por
/// eso NO se pinta nunca. Lo que se pinta es el título.
fn comandos_de(p: &PluginInfo) -> Vec<ExtensionCommandView> {
    p.commands
        .iter()
        .map(|c| {
            let (title, hostile) = norte_frontend::display_name(c.title.as_bytes());
            ExtensionCommandView {
                id: c.id.clone(),
                title: clamp_display(title),
                hostile,
            }
        })
        .collect()
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

/// Una cadena que escribió un TERCERO, lista para pintar: enmascarada,
/// acotada, y con la bandera de si lo pintado difiere de lo que dice.
///
/// Es `plugin_label` con su bandera —la misma función, no una copia— más el
/// recorte de pantalla de este host. Donde la decisión ES la cadena (aprobar
/// una capability), la bandera es parte de la pregunta.
pub(crate) fn texto_de_tercero(raw: &str) -> (String, bool) {
    let (pintable, hostil) = plugin_label_flagged(raw);
    (clamp_display(pintable), hostil)
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
