//! Lo que un panel de PLUGIN tiene vivo en el terminal: su último marco, el
//! estado opaco que el guest se guardó, y qué repintado está en vuelo (fase 3).
//!
//! El marco lo describe el guest y lo pinta [`crate::ui`]; aquí solo se guarda.
//! Lo que sí se decide aquí es CUÁNDO lo que hay deja de valer, y por eso la
//! firma de abajo existe.

use norte_frontend::frame::StyledFrame;
use norte_proto::VPath;

/// Lo que hace distinto un repintado de otro.
///
/// Un panel no se repinta «cada tanto»: se repinta cuando cambia algo que el
/// guest vería. Comparar la firma de lo que se está mirando con la de lo que se
/// pidió es lo que hace las dos cosas que importan — no pedir dos veces lo
/// mismo, y TIRAR la respuesta que llega cuando el hueco ya quiere otra cosa.
///
/// Es la regla de los previews (`PreviewFetch` guarda su ruta), no un contador
/// de época: aquí la identidad es lo que se pidió, y compararla dice por sí
/// sola si la respuesta sigue valiendo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Firma {
    /// QUÉ panel es: el kind entero, `plugin:<id>:<kind>`.
    ///
    /// Va en la firma porque un `SlotId` se REUTILIZA: los presets traen ids
    /// pequeños y fijos, así que cambiar de disposición puede poner el panel
    /// de otro plugin en el mismo hueco. Sin el kind, `hay_que_pedir` decía
    /// que no hacía falta pedir nada —el directorio y el tamaño coincidían— y
    /// el marco del plugin anterior se quedaba pintado, con sus zonas
    /// pulsables, bajo el título del nuevo.
    pub kind: String,
    /// El directorio que el panel mira.
    pub dir: VPath,
    /// Ancho útil en celdas, sin los bordes del marco.
    pub cols: u32,
    /// Alto útil en celdas, sin los bordes.
    pub rows: u32,
    /// El nombre de la fila bajo el cursor del listado al que sigue, si hay
    /// alguno. Va en la firma porque el guest lo recibe: un panel que habla de
    /// la fila señalada tiene que repintarse al señalar otra.
    pub cursor: Option<String>,
}

/// El panel de un plugin, entre repintados.
#[derive(Debug, Default)]
pub struct PanelRuntime {
    /// De QUÉ panel es lo que hay guardado aquí.
    ///
    /// Un `SlotId` se reutiliza —cambiar de disposición o restaurar la sesión
    /// trae ids pequeños y fijos—, así que el hueco puede pasar de un plugin a
    /// otro. Podar por el árbol no basta: el hueco sigue VIVO, solo que ahora
    /// es de otro. Sin esto, lo de A se heredaba para B: su marco —con sus
    /// zonas pulsables— bajo el título de B, y su estado opaco entregado a B
    /// en la primera petición.
    pub kind: Option<String>,
    /// El último marco que llegó. Se conserva mientras se pide el siguiente:
    /// un plugin lento deja la foto de antes, no un hueco en blanco.
    pub frame: Option<StyledFrame>,
    /// El estado opaco del guest, tal cual: vuelve en la siguiente petición y
    /// este proceso no lo mira nunca.
    ///
    /// Sobrevive al marco a propósito — es lo ÚNICO que se conserva entre
    /// llamadas; el permiso de leer no (la sesión de ubicación muere con cada
    /// llamada, en el core).
    pub state: Option<Vec<u8>>,
    /// La firma del marco que se está enseñando.
    pub mostrado: Option<Firma>,
    /// La firma de la petición en vuelo, si hay una.
    pub en_vuelo: Option<Firma>,
    /// La última firma que se INTENTÓ y volvió sin marco: la RPC falló, o
    /// ningún plugin consentido pinta ese panel.
    ///
    /// Sin esto, un intento vacío no dejaba rastro —`mostrado` seguía como
    /// estaba y `en_vuelo` se limpiaba—, así que la vuelta siguiente del bucle
    /// volvía a pedir lo mismo: una RPC por frame pintado, para siempre, sin
    /// nada en pantalla que lo explicara. Y pasa sin plugins hostiles: basta
    /// una disposición guardada que nombre un panel de un plugin que ya no
    /// está.
    pub intentado: Option<Firma>,
}

impl PanelRuntime {
    /// Pone este hueco al servicio de `kind`, tirando lo que fuera de otro.
    ///
    /// Un `SlotId` se REUTILIZA: cambiar de disposición o restaurar la sesión
    /// traen ids pequeños y fijos, así que el hueco 3 puede ser de un plugin
    /// hoy y de otro dentro de un segundo. Podar por el árbol no lo cubre —el
    /// hueco sigue vivo, solo que ahora es de otro—, y lo que había no se
    /// hereda: ni el marco, porque sus zonas pulsables seguirían respondiendo
    /// bajo el título del nuevo, ni el ESTADO OPACO, que es del primero y cuyo
    /// consentimiento el lector dio plugin a plugin.
    ///
    /// Devuelve si hubo relevo, para quien quiera decirlo.
    pub fn adoptar(&mut self, kind: &str) -> bool {
        if self.kind.as_deref() == Some(kind) {
            return false;
        }
        *self = Self {
            kind: Some(kind.to_owned()),
            ..Self::default()
        };
        true
    }

    /// ¿Hace falta pedir el marco de `firma`?
    ///
    /// No, si ya se está enseñando ese mismo, y no, si ya se pidió: un panel
    /// que se repide en cada frame haría una llamada al guest por pintado.
    #[must_use]
    pub fn hay_que_pedir(&self, firma: &Firma) -> bool {
        // Y tampoco lo que ya se intentó y volvió vacío: un panel sin plugin
        // que lo pinte no es un panel que haya que volver a pedir en cada
        // frame. Cuando cambie algo que el guest vería, la firma será otra y
        // se intentará de nuevo.
        self.mostrado.as_ref() != Some(firma)
            && self.en_vuelo.as_ref() != Some(firma)
            && self.intentado.as_ref() != Some(firma)
    }
}

/// Pide el marco del panel de plugin visible, si hace falta (fase 3).
///
/// Se llama una vez por turno de pintado, que es lo que le da su cadencia: no
/// hay temporizador ni coalescedor: hay UNA petición viva por hueco y la
/// siguiente sustituye a la anterior, soltando su receptor —la misma regla que
/// el preview acoplado—. Lo que decide si hace falta es la firma, no el reloj.
pub fn pedir_marco(
    app: &mut crate::app::App,
    backend: &norte_core::backend::Backend,
    work: &mut crate::jobs::InFlight,
    painted: ratatui::layout::Rect,
) {
    // Las guardas ANTES de la geometría: resolver el árbol es un reparto
    // entero, y la pantalla que no tiene ningún panel de plugin —que son casi
    // todas— no debe pagarlo por frame.
    let Some(slot) = app.panel_slot() else {
        return;
    };
    let Some(kind) = app.layout.kind_of(slot).map(|k| k.as_str().to_owned()) else {
        return;
    };
    let Some((plugin_id, panel_kind)) = partes(&kind) else {
        return;
    };
    // Y una petición viva por hueco: mientras una está en vuelo no se empieza
    // otra. Soltar el receptor descartaba la RESPUESTA, no el trabajo — el
    // guest se instancia y corre igual—, así que arrastrar un borde encolaba
    // una instanciación de wasm por frame. La siguiente vuelta del bucle
    // vuelve a mirar, así que lo que se pierde es una vuelta, no el repintado.
    if app.paneles.entry(slot).en_vuelo.is_some() {
        return;
    }
    let res = crate::ui::resolved_for(app, painted);
    let Some(rect) = crate::ui::slot_rect(&res, slot) else {
        return;
    };
    let rect = crate::ui::contenido_de_hueco(&app.layout, slot, rect);
    let firma = Firma {
        kind: kind.clone(),
        dir: app.focused().dir().clone(),
        // Sin los bordes: el guest describe lo de DENTRO, y darle el tamaño
        // con marco le haría contar con dos celdas que no son suyas.
        cols: u32::from(rect.width.saturating_sub(2)),
        rows: u32::from(rect.height.saturating_sub(2)),
        // `cursor_entry` y no `selected`, por lo mismo que el visor acoplado y
        // la hoja de atributos: el panel habla de lo que hay BAJO el cursor,
        // no de lo que esté marcado.
        // Por `display_name` y no por `from_utf8_lossy`: es el saneado que usa
        // todo lo que se pinta, así que el guest recibe el nombre que esta casa
        // enseñaría. Y distingue dos nombres que solo difieren en bytes
        // inválidos, que el lossy colapsaba — con él, mover el cursor entre
        // esos dos no cambiaba la firma y el panel no se repintaba.
        cursor: app.focused().cursor_entry().and_then(|e| {
            e.path
                .file_name()
                .map(|n| norte_frontend::display_name(n.as_bytes()).0)
        }),
    };
    // Y que el kind esté DECLARADO por un plugin consentido: el prefijo lo
    // escribe quien edite una disposición, y sin esta puerta un
    // `plugin:loquesea:loquesea` en un fichero bastaba para pedirle al core
    // que resolviera con el directorio que el lector está mirando.
    if !app.kinds.decls().iter().any(|d| d.id.as_str() == kind) {
        return;
    }
    app.paneles.entry(slot).adoptar(&kind);
    if !app.paneles.entry(slot).hay_que_pedir(&firma) {
        return;
    }
    let params = norte_proto::methods::PluginPanelRenderParams {
        plugin_id: plugin_id.to_owned(),
        kind: panel_kind.to_owned(),
        dir: firma.dir.clone(),
        cols: firma.cols,
        rows: firma.rows,
        lang: norte_frontend::frame::lang_code().to_owned(),
        cursor_name: firma.cursor.clone(),
        // Lo que el guest se guardó la última vez, tal cual: este proceso no lo
        // mira.
        state: app.paneles.entry(slot).state.clone(),
        // SIEMPRE el evento neutro, hoy: ningún frontend manda todavía `Click`
        // ni `Command` al guest. Una zona pulsada ejecuta un comando del
        // catálogo (`mouse::zona_de_panel_en`) y el guest no se entera; que
        // pueda reaccionar a sus propias zonas es lo que falta, y el sitio es
        // esta línea.
        event: norte_proto::methods::PanelEvent::Refresh,
    };
    app.paneles.entry(slot).en_vuelo = Some(firma.clone());
    work.panel_render = Some(crate::probes::spawn_panel_render(
        backend, slot, firma, params,
    ));
}

/// Aplica el marco que volvió del core, si sigue valiendo.
///
/// Tres formas de no aplicarlo, y las tres dejan lo que hubiera:
/// - la respuesta es de una petición VIEJA (el hueco ya quiere otra cosa),
/// - la llamada falló,
/// - ningún plugin consentido pinta ese panel.
///
/// En los tres casos se conserva el marco anterior: un plugin lento o roto
/// deja la foto de antes, nunca un hueco que parpadea a vacío.
pub fn aterrizar(
    app: &mut crate::app::App,
    slot: norte_frontend::layout::SlotId,
    firma: &Firma,
    res: Option<Result<Option<norte_proto::methods::PanelFrame>, norte_proto::Error>>,
) {
    let panel = app.paneles.entry(slot);
    // La respuesta de una petición que ya no es la viva no se aplica ni limpia
    // nada: la que está en vuelo es otra y es la que manda.
    if panel.en_vuelo.as_ref() != Some(firma) {
        return;
    }
    panel.en_vuelo = None;
    // El intento queda anotado PASE LO QUE PASE: es lo que impide que un panel
    // sin marco se repida en cada pintado.
    panel.intentado = Some(firma.clone());
    let Some(Ok(Some(marco))) = res else {
        return;
    };
    // Y que lo firme QUIEN se pidió: el marco dice de qué plugin es, y un
    // `plugin_id` que no es el del kind de esta firma no se pinta. Con la
    // firma llevando el kind esto no debería poder pasar; se comprueba porque
    // el dato viene de fuera y comprobarlo cuesta una línea.
    if partes(&firma.kind).map(|(id, _)| id) != Some(marco.plugin_id.as_str()) {
        return;
    }
    panel.frame = Some(marco_de_wire(&marco));
    panel.state = marco.state;
    panel.mostrado = Some(firma.clone());
}

/// El marco del wire, ACOTADO y SANEADO, en la forma que pinta el terminal.
///
/// Por `clamped` y no campo a campo: los topes son los del protocolo y el
/// recorte tira las zonas que apuntan a una línea que no se pinta. Un guest
/// hostil manda mil líneas y diez mil zonas igual que uno honesto manda ocho.
///
/// Y por [`norte_frontend::ansi::span_de_wire`] y no copiando los campos: el
/// texto de un tramo es de un TERCERO y se enmascara igual que el de una
/// preview estilada, y su `role` se valida contra lo que un plugin puede
/// pedir. Copiarlos a mano —como estaba— dejaba pasar escapes de terminal y
/// roles del cromo por el único camino que no había mirado nadie.
fn marco_de_wire(marco: &norte_proto::methods::PanelFrame) -> StyledFrame {
    StyledFrame::de_wire(marco)
}

/// `plugin:<id>:<kind>` partido en las dos mitades que necesita la RPC.
///
/// El separador es el PRIMER `:` tras el prefijo, y eso es seguro porque el
/// alfabeto que valida `KindRegistry::insert_panels` no deja pasar dos puntos
/// ni en el id ni en el kind: sin esa puerta, un plugin llamado `a:b` podría
/// hacerse pasar por el panel de otro.
///
/// ```
/// # use norte_tui::panelplugin::partes;
/// assert_eq!(partes("plugin:git:status"), Some(("git", "status")));
/// assert_eq!(partes("browser"), None);
/// ```
#[must_use]
pub fn partes(kind: &str) -> Option<(&str, &str)> {
    kind.strip_prefix("plugin:")?.split_once(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn firma(dir: &str, cursor: Option<&str>) -> Firma {
        Firma {
            kind: "plugin:git:status".to_owned(),
            dir: VPath::parse(dir).expect("wire"),
            cols: 30,
            rows: 8,
            cursor: cursor.map(str::to_owned),
        }
    }

    /// Lo que ya se enseña no se vuelve a pedir, y lo que ya se pidió tampoco.
    ///
    /// Sin esto, el terminal repinta en cada vuelta del bucle y cada vuelta
    /// sería una llamada al guest: un panel de git ejecutando wasm sesenta
    /// veces por segundo para enseñar la misma rama.
    #[test]
    fn no_se_repide_lo_que_ya_se_ensena_ni_lo_que_ya_se_pidio() {
        let f = firma("mem:///a", None);
        let mut p = PanelRuntime::default();
        assert!(p.hay_que_pedir(&f), "sin nada, hay que pedir");

        p.en_vuelo = Some(f.clone());
        assert!(!p.hay_que_pedir(&f), "ya está pedido");

        p.en_vuelo = None;
        p.mostrado = Some(f.clone());
        assert!(!p.hay_que_pedir(&f), "ya se está enseñando");
    }

    /// Mover el cursor cambia la firma: el guest recibe la fila señalada, así
    /// que señalar otra es otro marco.
    #[test]
    fn mover_el_cursor_pide_otro_marco() {
        let p = PanelRuntime {
            mostrado: Some(firma("mem:///a", Some("uno"))),
            ..Default::default()
        };
        assert!(p.hay_que_pedir(&firma("mem:///a", Some("dos"))));
        assert!(p.hay_que_pedir(&firma("mem:///b", Some("uno"))));
    }

    /// Un intento que vuelve VACÍO no se repite en el frame siguiente.
    ///
    /// Sin esto, un panel cuyo plugin ya no está —una disposición guardada que
    /// lo nombra— pedía un marco por cada pintado: una RPC por frame, y
    /// embebido un escaneo del catálogo en disco con ella.
    #[test]
    fn un_intento_vacio_no_se_repide_en_cada_frame() {
        let f = firma("mem:///a", None);
        let mut p = PanelRuntime {
            en_vuelo: Some(f.clone()),
            ..Default::default()
        };
        p.en_vuelo = None;
        p.intentado = Some(f.clone());
        assert!(!p.hay_que_pedir(&f), "ya se intentó y volvió sin marco");
        // Pero lo que cambia el contexto sí se pide: el plugin pudo volver, y
        // de todas formas el guest vería otra cosa.
        assert!(p.hay_que_pedir(&firma("mem:///b", None)));
    }

    /// Un hueco que pasa a ser de OTRO plugin no hereda nada del anterior.
    ///
    /// Ni el marco —sus zonas pulsables seguirían respondiendo bajo el título
    /// del nuevo— ni el estado opaco, que es del primero. Pasa al cambiar de
    /// disposición o al restaurar la sesión, porque los ids de hueco de un
    /// preset son pequeños y fijos.
    #[test]
    fn un_hueco_que_cambia_de_plugin_no_hereda_nada() {
        let mut p = PanelRuntime::default();
        assert!(p.adoptar("plugin:git:status"), "estrena hueco");
        p.frame = Some(StyledFrame::clamped(Vec::new(), Vec::new()));
        p.state = Some(b"lo de git".to_vec());
        p.mostrado = Some(firma("mem:///a", None));

        assert!(!p.adoptar("plugin:git:status"), "el mismo panel no releva");
        assert!(p.state.is_some(), "y no tira lo suyo");

        assert!(p.adoptar("plugin:otro:cosas"), "otro plugin sí releva");
        assert!(
            p.state.is_none(),
            "el estado opaco del primero no se hereda"
        );
        assert!(p.frame.is_none(), "ni su marco, con sus zonas");
        assert!(p.mostrado.is_none(), "y vuelve a pedir");
    }

    /// Un kind de casa no se parte: no es de ningún plugin.
    #[test]
    fn solo_se_parten_los_kinds_de_plugin() {
        assert_eq!(
            partes("plugin:acme.git:status"),
            Some(("acme.git", "status"))
        );
        assert_eq!(partes("plugin:sinkind"), None);
        assert_eq!(partes("logview"), None);
    }
}
