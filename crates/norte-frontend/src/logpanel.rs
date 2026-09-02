//! El panel de registro: qué se enseña del anillo y cómo se recorre.
//!
//! El anillo (`norte_config::logring`) guarda; esto decide qué se ve. Vive en
//! la crate compartida porque la ventana necesita las mismas respuestas, y una
//! decisión de presentación duplicada entre frontends diverge en silencio
//! (ADR 0077).
//!
//! # Dos niveles, y confundirlos es la trampa
//!
//! Hay el nivel que el anillo **captura** y el nivel que el panel **enseña**, y
//! no son el mismo. Filtrar a DEBUG lo que se guardó a INFO no enseña nada:
//! los DEBUG no existen. Quien cambia el que se enseña tiene que subir el del
//! anillo, y ese invariante vive en `LogRing::raise_to` —en el tipo que POSEE
//! el nivel— y no en un valor de retorno que un segundo frontend pueda ignorar.
//!
//! Y bajarlo NO baja el del anillo, a propósito: ir a DEBUG, volver a WARN y
//! pedir DEBUG otra vez tiene que enseñar lo de en medio. Si al bajar dejáramos
//! de capturar, ese viaje de ida y vuelta borraría justo el rato que se estaba
//! investigando. Se paga capturando de más mientras dure la sesión, que es lo
//! barato de las dos equivocaciones posibles.

use norte_config::logline::{LogLevel, LogLine};

/// De dónde salen las líneas que el panel enseña.
///
/// Esto es la PREFERENCIA guardada, no lo que se pinta. Un frontend con el
/// core embebido —un solo proceso, un solo anillo— no tiene una segunda
/// fuente que mostrar, y quien decide reducir `Both` a `Window` y esconder el
/// selector en ese caso es ESE frontend: aquí no hay manera de saber si hay un
/// daemon al otro lado, y esta crate no debe fingir que la hay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LogSource {
    /// Solo este proceso.
    Window,
    /// Solo el daemon.
    Daemon,
    /// Los dos, mezclados por marca de tiempo.
    #[default]
    Both,
}

/// Estado del panel.
#[derive(Debug, Clone)]
pub struct LogPanel {
    /// Hasta qué verbosidad se ENSEÑA.
    level: LogLevel,
    /// Qué fuente se enseña: preferencia, no lo pintado (ver [`LogSource`]).
    source: LogSource,
    /// Filtro de texto sobre módulo y mensaje. Vacío = todo.
    filter: String,
    /// El mismo, ya en minúsculas.
    ///
    /// Precalculado porque [`Self::matches`] corre una vez POR LÍNEA y por
    /// frame: con la aguja dentro, cada llamada reservaba una `String` nueva
    /// para lo mismo, dos mil veces, diez veces por segundo.
    filter_lc: String,
    /// Cuántas líneas hay por encima de la primera visible. `None` = pegado al
    /// final (sigue lo que llega).
    scroll: Option<usize>,
    /// Cuántas filas caben, del frame anterior.
    ///
    /// Lo pone quien pinta, y esto se corrigió tras una revisión: el manejo de
    /// teclas ADIVINABA diez —el alto con el que se abre el hueco— mientras el
    /// pintado usaba el interior real, que son ocho. Cada página se saltaba dos
    /// líneas y la primera, cuatro: lo que ninguna de las dos ventanas enseñaba
    /// no se podía leer de ninguna manera. Adivinar el viewport rompe el scroll
    /// en silencio, y este árbol ya tenía el remedio para las otras listas
    /// largas (`ui::geometry::before_frame`).
    rows: usize,
}

impl Default for LogPanel {
    fn default() -> Self {
        Self {
            // INFO: lo mismo que captura el anillo al arrancar, así que abrir
            // el panel enseña algo desde el primer momento.
            level: LogLevel::Info,
            source: LogSource::Both,
            filter: String::new(),
            filter_lc: String::new(),
            scroll: None,
            // Uno hasta que el primer frame diga la verdad: nunca cero, para
            // que una página antes de pintar mueva algo en vez de nada.
            rows: 1,
        }
    }
}

impl LogPanel {
    /// El nivel que se está enseñando.
    #[must_use]
    pub const fn level(&self) -> LogLevel {
        self.level
    }

    /// Cambia el nivel que se ENSEÑA.
    ///
    /// Subir el del anillo es cosa de `LogRing::raise_to`, y esto se corrigió
    /// tras una revisión: antes esta función devolvía «el nivel que el anillo
    /// debe capturar» y confiaba en que el llamante lo comparase y subiera.
    /// `#[must_use]` obliga a atar el valor, no a usarlo — y el segundo
    /// frontend habría copiado un `let _ =` y acabado filtrando a DEBUG unas
    /// líneas que nadie capturó. El invariante vive ahora en el tipo que posee
    /// el nivel.
    pub fn show_level(&mut self, l: LogLevel) {
        self.level = l;
        // Volver al final: tras cambiar el filtro, lo que el lector quiere ver
        // es lo último que encaja, no el trozo donde estaba mirando de otra
        // lista.
        self.scroll = None;
    }

    /// La fuente que se está enseñando (preferencia, ver [`LogSource`]).
    #[must_use]
    pub const fn source(&self) -> LogSource {
        self.source
    }

    /// Cambia la fuente.
    pub fn set_source(&mut self, s: LogSource) {
        self.source = s;
        // Igual que al cambiar de filtro o de nivel: la lista compuesta
        // cambia de forma, y quedarse en el desplazamiento de la ANTERIOR
        // deja al lector en un trozo que no pidió.
        self.scroll = None;
    }

    /// Recorre las tres fuentes y vuelve a la primera: es UN mando, no tres.
    pub fn cycle_source(&mut self) {
        self.set_source(match self.source {
            LogSource::Window => LogSource::Daemon,
            LogSource::Daemon => LogSource::Both,
            LogSource::Both => LogSource::Window,
        });
    }

    /// El filtro de texto actual.
    #[must_use]
    pub fn filter(&self) -> &str {
        &self.filter
    }

    /// Cambia el filtro de texto.
    pub fn set_filter(&mut self, f: impl Into<String>) {
        self.filter = f.into();
        self.filter_lc = self.filter.to_lowercase();
        self.scroll = None;
    }

    /// ¿Está pegado al final?
    #[must_use]
    pub const fn following(&self) -> bool {
        self.scroll.is_none()
    }

    /// Vuelve a pegarse al final.
    pub const fn follow(&mut self) {
        self.scroll = None;
    }

    /// Cuántas filas caben. Lo llama quien pinta, una vez por frame.
    pub const fn set_viewport_rows(&mut self, rows: usize) {
        // Nunca cero: con cero, `tope` sería el total y una página no movería
        // nada.
        self.rows = if rows == 0 { 1 } else { rows };
    }

    /// Cuántas filas caben (las del último frame).
    #[must_use]
    pub const fn viewport_rows(&self) -> usize {
        self.rows
    }

    /// Sube `n` líneas, despegándose del final.
    ///
    /// Despegarse es la mitad del panel: uno que salta siempre al final no se
    /// puede leer mientras algo escribe, que es justo cuando hace falta.
    pub fn scroll_up(&mut self, n: usize, visibles: usize) {
        let tope = visibles.saturating_sub(self.rows);
        let actual = self.scroll.unwrap_or(tope);
        self.scroll = Some(actual.saturating_sub(n));
    }

    /// Baja `n` líneas; al llegar al final se vuelve a pegar.
    pub fn scroll_down(&mut self, n: usize, visibles: usize) {
        let tope = visibles.saturating_sub(self.rows);
        let actual = self.scroll.unwrap_or(tope);
        let nuevo = actual.saturating_add(n);
        self.scroll = if nuevo >= tope { None } else { Some(nuevo) };
    }

    /// ¿Pasa esta línea los dos filtros?
    ///
    /// El texto se compara en minúsculas y contra el módulo TAMBIÉN, no solo
    /// contra el mensaje: media búsqueda real es «enséñame lo de connect».
    #[must_use]
    pub fn matches(&self, line: &LogLine) -> bool {
        if line.level > self.level {
            return false;
        }
        if self.filter_lc.is_empty() {
            return true;
        }
        contiene_sin_mayusculas(&line.message, &self.filter_lc)
            || contiene_sin_mayusculas(&line.target, &self.filter_lc)
    }

    /// Las líneas que se ven, en orden, y desde qué índice empieza la ventana
    /// de `alto` filas.
    ///
    /// Devuelve índices sobre el filtrado y no sobre el anillo: el llamante
    /// pinta un trozo, y lo que se recorta es lo que se ve, no lo que hay.
    #[must_use]
    pub fn view<'a>(&self, lines: &'a [LogLine], alto: usize) -> (Vec<&'a LogLine>, usize) {
        let visibles: Vec<&LogLine> = lines.iter().filter(|l| self.matches(l)).collect();
        let desde = self.window_start(visibles.len(), alto);
        (visibles, desde)
    }

    /// Desde qué índice empieza la ventana de `alto` filas sobre una lista ya
    /// filtrada de `total` elementos.
    ///
    /// La otra mitad de [`Self::view`], expuesta aparte porque quien mezcla dos
    /// fuentes ([`merge`]) ya no tiene un `&[LogLine]` que darle: tiene parejas
    /// `(línea, origen)`. Sin esto, ese llamante tenía que materializar la
    /// mezcla en un `Vec<LogLine>` propio —clonando lo que `merge` presta a
    /// propósito— solo para volver a preguntar por el desplazamiento.
    ///
    /// ```
    /// use norte_frontend::logpanel::LogPanel;
    /// let mut p = LogPanel::default();
    /// p.set_viewport_rows(10);
    /// // Pegado al final: la ventana empieza donde caben las diez últimas.
    /// assert_eq!(p.window_start(25, 10), 15);
    /// // Y nunca por encima del tope, aunque la lista encoja debajo.
    /// assert_eq!(p.window_start(4, 10), 0);
    /// ```
    #[must_use]
    pub fn window_start(&self, total: usize, alto: usize) -> usize {
        let tope = total.saturating_sub(alto);
        self.scroll.map_or(tope, |s| s.min(tope))
    }

    /// Cuántas líneas del anillo pasan los filtros.
    ///
    /// Sin construir el vector: es lo único que el desplazamiento necesita, y
    /// hacerlo con [`Self::view`] clonaba referencias de las 2000 en cada
    /// tecla, incluidas las que no desplazan nada.
    #[must_use]
    pub fn visible_count(&self, lines: &[LogLine]) -> usize {
        lines.iter().filter(|l| self.matches(l)).count()
    }
}

/// Mezcla dos listas ya ordenadas por `epoch_ms`, marcando el origen de cada
/// línea. Estable: a igual marca, primero la local — dos procesos en una
/// misma máquina comparten reloj, así que las marcas iguales son el caso
/// normal, no el raro, y una lista que se reordena entre frames no se puede
/// leer.
///
/// Devuelve líneas PRESTADAS a propósito: el anillo ya clonó una vez en su
/// `snapshot`, y el panel pinta como mucho una pantalla; clonar dos mil
/// líneas otra vez por frame es justo el gasto que la proyección de la
/// ventana se escribió para evitar.
///
/// El filtro de nivel y el de texto NO se aplican aquí: van después, sobre el
/// resultado, para que una línea del daemon no se cuele por venir de fuera.
#[must_use]
pub fn merge<'a>(
    local: &'a [LogLine],
    remote: &'a [LogLine],
    s: LogSource,
) -> Vec<(&'a LogLine, LogSource)> {
    match s {
        LogSource::Window => local.iter().map(|l| (l, LogSource::Window)).collect(),
        LogSource::Daemon => remote.iter().map(|l| (l, LogSource::Daemon)).collect(),
        LogSource::Both => {
            let mut out = Vec::with_capacity(local.len() + remote.len());
            let mut i = 0;
            let mut j = 0;
            while i < local.len() && j < remote.len() {
                if remote[j].epoch_ms < local[i].epoch_ms {
                    out.push((&remote[j], LogSource::Daemon));
                    j += 1;
                } else {
                    // Igual marca: la local primero, a propósito.
                    out.push((&local[i], LogSource::Window));
                    i += 1;
                }
            }
            out.extend(local[i..].iter().map(|l| (l, LogSource::Window)));
            out.extend(remote[j..].iter().map(|l| (l, LogSource::Daemon)));
            out
        }
    }
}

/// ¿Contiene `heno` la `aguja` (que YA viene en minúsculas), sin distinguir
/// mayúsculas y sin reservar memoria?
///
/// `heno.to_lowercase().contains(..)` copiaba el mensaje entero por línea y por
/// frame. Esto compara ventana a ventana sobre el original.
fn contiene_sin_mayusculas(heno: &str, aguja: &str) -> bool {
    if aguja.is_empty() {
        return true;
    }
    // Por CARACTERES en minúscula y no por bytes: `char::to_lowercase` puede
    // dar más de uno (la `İ` turca), y comparar bytes crudos fallaría en cuanto
    // el mensaje llevara acentos. Y sin `collect`: recolectar en dos `Vec` para
    // comparar ventanas reservaría DOS veces por línea, que es peor que el
    // `to_lowercase` que esto vino a quitar.
    for (i, _) in heno.char_indices() {
        let mut h = heno[i..].chars().flat_map(char::to_lowercase);
        let mut a = aguja.chars();
        loop {
            match (a.next(), h.next()) {
                // Se acabó la aguja sin discrepar: está.
                (None, _) => return true,
                // Se acabó el heno antes que la aguja: no cabe, y desde una
                // posición más adelante tampoco cabría.
                (Some(_), None) => return false,
                (Some(ac), Some(hc)) if ac == hc => {}
                _ => break,
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    fn l(level: LogLevel, target: &str, msg: &str) -> LogLine {
        LogLine {
            epoch_ms: 0,
            level,
            target: target.into(),
            message: msg.into(),
        }
    }

    fn corpus() -> Vec<LogLine> {
        vec![
            l(LogLevel::Info, "norte_core::connect", "conectando"),
            l(LogLevel::Warn, "norte_core::connect", "fallo de conexión"),
            l(LogLevel::Debug, "norte_tui::fill", "página 2"),
            l(LogLevel::Error, "norte_core::journal", "no se pudo anclar"),
        ]
    }

    /// El filtro de nivel deja pasar lo MENOS verboso, no solo lo igual: pedir
    /// WARN y perder los ERROR sería enseñar menos cuanto peor va la cosa.
    #[test]
    fn el_nivel_deja_pasar_lo_mas_grave() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Warn);
        let v: Vec<_> = corpus().into_iter().filter(|x| p.matches(x)).collect();
        assert_eq!(v.len(), 2, "{v:?}");
        assert!(v.iter().all(|x| x.level <= LogLevel::Warn));
    }

    /// El texto busca también en el MÓDULO: «enséñame lo de connect» es media
    /// búsqueda real, y sin esto habría que saberse los mensajes de memoria.
    #[test]
    fn el_texto_busca_en_el_modulo_y_en_el_mensaje() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Trace);
        p.set_filter("connect");
        assert_eq!(corpus().iter().filter(|x| p.matches(x)).count(), 2);
        p.set_filter("ANCLAR"); // sin distinguir mayúsculas
        assert_eq!(corpus().iter().filter(|x| p.matches(x)).count(), 1);
        p.set_filter("");
        assert_eq!(corpus().iter().filter(|x| p.matches(x)).count(), 4);
    }

    /// Pedir más detalle devuelve el nivel que el anillo tiene que capturar:
    /// filtrar a DEBUG lo que se guardó a INFO no enseña nada.
    #[test]
    fn pedir_debug_dice_lo_que_hay_que_capturar() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Debug);
        assert_eq!(p.level(), LogLevel::Debug);
    }

    /// Arranca pegado al final; subir lo despega; bajar hasta el final lo
    /// vuelve a pegar. Un panel que salta siempre al final no se puede leer
    /// mientras algo escribe, que es cuando hace falta.
    #[test]
    fn seguir_el_final_se_suelta_al_subir_y_se_recupera_al_bajar() {
        let mut p = LogPanel::default();
        assert!(p.following());
        p.set_viewport_rows(4);
        p.scroll_up(1, 10);
        assert!(!p.following(), "subir no soltó el seguimiento");
        p.scroll_down(99, 10);
        assert!(p.following(), "llegar al final no volvió a pegarlo");
    }

    /// La ventana se recorta sobre lo FILTRADO: con cuatro líneas, un filtro
    /// que deja dos y un alto de una, se ve la última de las dos.
    #[test]
    fn la_ventana_se_recorta_sobre_lo_filtrado() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Warn);
        let lineas = corpus();
        let (visibles, desde) = p.view(&lineas, 1);
        assert_eq!(visibles.len(), 2);
        assert_eq!(desde, 1, "pegado al final, empieza en la última");
        assert_eq!(visibles[desde].message, "no se pudo anclar");
    }

    /// El alto lo pone quien pinta, y las páginas cuadran con ÉL.
    ///
    /// Esto es el arreglo de un fallo de verdad: el manejo de teclas adivinaba
    /// diez filas y el pintado usaba ocho, así que cada página se saltaba dos
    /// líneas y la primera, cuatro. Con 100 líneas y 8 filas, pegado al final
    /// se ven de la 92 a la 99; una página arriba tiene que enseñar de la 84 a
    /// la 91 — sin huecos entre las dos ventanas.
    #[test]
    fn una_pagina_no_se_salta_ninguna_linea() {
        let mut p = LogPanel::default();
        p.set_viewport_rows(8);
        let lineas: Vec<LogLine> = (0..100)
            .map(|i| l(LogLevel::Info, "t", &format!("linea {i}")))
            .collect();

        let (_, desde) = p.view(&lineas, 8);
        assert_eq!(desde, 92, "pegado al final empieza en 92");

        p.scroll_up(8, 100);
        let (_, desde) = p.view(&lineas, 8);
        assert_eq!(
            desde, 84,
            "la página anterior tiene que empezar justo donde acaba la de abajo"
        );
    }

    /// Con la aguja precalculada, buscar sigue sin distinguir mayúsculas y
    /// sigue funcionando con acentos — que es lo que se rompería al comparar
    /// bytes crudos en vez de caracteres.
    #[test]
    fn el_filtro_sin_reservar_sigue_encontrando_acentos() {
        let mut p = LogPanel::default();
        p.set_filter("CONEXIÓN");
        assert!(p.matches(&l(LogLevel::Warn, "t", "fallo de conexión remota")));
        p.set_filter("ó");
        assert!(p.matches(&l(LogLevel::Warn, "t", "CONEXIÓN")));
        p.set_filter("zzz");
        assert!(!p.matches(&l(LogLevel::Warn, "t", "conexión")));
        // Una aguja más larga que el heno no puede «encontrarse».
        p.set_filter("larguísima aguja");
        assert!(!p.matches(&l(LogLevel::Warn, "t", "corto")));
    }

    /// Cambiar de filtro vuelve al final: quedarse en el desplazamiento de la
    /// lista ANTERIOR deja al lector en un trozo que no pidió.
    #[test]
    fn cambiar_de_filtro_vuelve_al_final() {
        let mut p = LogPanel::default();
        p.set_viewport_rows(3);
        p.scroll_up(2, 10);
        assert!(!p.following());
        p.set_filter("x");
        assert!(p.following());
        p.set_viewport_rows(3);
        p.scroll_up(2, 10);
        p.show_level(LogLevel::Error);
        assert!(p.following());
    }

    /// Línea con marca de tiempo, para las pruebas de mezcla. Distinto de
    /// [`l`] (que fija `epoch_ms` a 0 y pide nivel y módulo) porque `merge`
    /// solo le importa la marca y el mensaje.
    fn le(ms: i64, msg: &str) -> LogLine {
        LogLine {
            epoch_ms: ms,
            level: LogLevel::Info,
            target: "t".into(),
            message: msg.into(),
        }
    }

    /// La mezcla respeta el reloj, y a igual marca no baila: primero la local.
    #[test]
    fn la_mezcla_ordena_por_marca_y_es_estable() {
        let local = vec![le(10, "ventana-a"), le(30, "ventana-b")];
        let remoto = vec![le(10, "daemon-a"), le(20, "daemon-b")];
        let m = merge(&local, &remoto, LogSource::Both);
        let ms: Vec<_> = m.iter().map(|(l, _)| l.message.as_str()).collect();
        assert_eq!(ms, ["ventana-a", "daemon-a", "daemon-b", "ventana-b"]);
        assert_eq!(m[1].1, LogSource::Daemon);
    }

    /// Elegir una fuente NO mezcla: enseña esa y nada más.
    #[test]
    fn una_fuente_sola_no_trae_la_otra() {
        let local = vec![le(10, "ventana")];
        let remoto = vec![le(20, "daemon")];
        assert_eq!(merge(&local, &remoto, LogSource::Window).len(), 1);
        assert_eq!(
            merge(&local, &remoto, LogSource::Daemon)[0].0.message,
            "daemon"
        );
    }

    /// El ciclo recorre las tres y vuelve: es UN mando, no tres.
    #[test]
    fn el_ciclo_de_fuente_da_la_vuelta() {
        let mut p = LogPanel::default();
        assert_eq!(p.source(), LogSource::Both);
        p.cycle_source();
        p.cycle_source();
        p.cycle_source();
        assert_eq!(p.source(), LogSource::Both);
    }

    /// El filtro de nivel y el de texto siguen aplicándose DESPUÉS de mezclar:
    /// una línea del daemon que no pasa el filtro no se cuela por venir de
    /// fuera.
    #[test]
    fn el_filtro_manda_tambien_sobre_lo_remoto() {
        let mut p = LogPanel::default();
        p.show_level(LogLevel::Error);
        let remoto = vec![LogLine {
            epoch_ms: 1,
            level: LogLevel::Debug,
            target: "norte_core".into(),
            message: "ruido".into(),
        }];
        let m = merge(&[], &remoto, LogSource::Both);
        assert!(!p.matches(m[0].0));
    }
}
