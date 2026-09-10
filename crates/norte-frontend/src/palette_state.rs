//! La paleta de comandos: su modelo, el filtrado y el cursor.
//!
//! Izado de `norte-tui` (misma operación que `History`/`Trail` en la tarea
//! 2.3 del plan multi-frontend): filtrar una lista de comandos por lo
//! tecleado, mover el cursor entre lo que casa y saber qué está
//! seleccionado son REGLAS DE PRESENTACIÓN, y dos frontends con dos copias
//! son dos paletas que se comportan distinto sin que nadie lo note (ADR
//! 0066, decisión D14).
//!
//! Las FILAS las construye cada frontend con su propia lista de comandos
//! implementados; lo que se comparte es qué hace la paleta con ellas.

/// La paleta de comandos (`Ctrl+P`, vim `:`): filtro libre sobre las filas
/// que le dé el frontend.
///
/// Sus teclas NO resuelven contra el contexto `dialog`: es un editor de
/// texto libre, como el buscador incremental. No hay vocabulario `dialog.*`
/// para «teclear un carácter» o «correr lo seleccionado», así que quien la
/// tenga abierta trata esas teclas como fijas.
///
/// Las `rows` llegan YA construidas (el `build_rows` de cada frontend,
/// precomputadas como `help_lines`/`dialog_hints` —
/// mismo criterio: reconstruidas en el arranque y en cada hot-reload OK,
/// ANTES de que los efectivos se muevan al `Resolver`); `Palette::new` solo
/// pliega el haystack de cada fila. Mismo patrón de cache que
/// [`crate::nav::QuickSearch`] (#77): el fold por fila se computa UNA vez
/// aquí, no por keystroke — los keystrokes solo pliegan la query.
#[derive(Debug, Clone)]
pub struct Palette {
    /// `(comando, descripción, chord-o-guion)` — snapshot congelado al abrir.
    rows: Vec<crate::palette::Row>,
    /// Haystack plegado por fila (nombre + descripción, [`crate::nav::fold`]),
    /// índice-paralelo a `rows`.
    folds: Vec<String>,
    /// Bytes tecleados tal cual (matching SIN sanear; el saneado es solo al
    /// pintar, [`Self::query_display`] — mismo contrato que
    /// [`crate::nav::QuickSearch::query_display`]).
    query: Vec<u8>,
    /// Índices REALES en `rows` que casan (query vacía = todas).
    visible: Vec<usize>,
    /// Posición de la selección DENTRO de `visible`.
    cursor: usize,
    /// Claves lanzadas hace poco, la más reciente primero (spec 2026-09-10):
    /// con la consulta vacía van arriba, en ese orden. Vienen de la sesión
    /// ([`crate::session::SessionBody::palette_recent`]).
    recent: Vec<String>,
}

/// ¿Es `needle` subsecuencia de `hay`? (`cpf` casa `copy path` porque `c`,
/// `p`, `f`... — sí, `f` no: casa `cop` y `pat`; lo que importa es que cada
/// byte aparezca en orden). Vacío casa todo. Solo bytes plegados.
fn is_subsequence(needle: &str, hay: &str) -> bool {
    // Por CHARS, no por bytes: una consulta no puede casar sobre un byte de
    // continuación en mitad de un carácter (revisión m12).
    let mut it = hay.chars();
    needle.chars().all(|c| it.any(|h| h == c))
}

impl Palette {
    /// [`Self::new`] con los comandos recientes: con la consulta vacía, las
    /// filas cuya clave esté en `recent` van primero, en el orden de
    /// `recent`. Una clave que ya no tiene fila (un plugin desinstalado, un
    /// comando que este frontend no implementa) no pinta nada.
    #[must_use]
    pub fn with_recent(rows: Vec<crate::palette::Row>, recent: &[String]) -> Self {
        let mut p = Self::new(rows);
        p.recent = recent.to_vec();
        p.recompute();
        p
    }
    /// Abre la palette sobre `rows` (la snapshot precomputada de `App`):
    /// pliega el haystack de cada fila y arranca con la query vacía (todo
    /// visible). El fold es sobre `text`+`desc` (lo PINTADO, ya enmascarado
    /// para una fila de plugin) — jamás sobre `key` (P1: podría llevar el
    /// `command_id` crudo del manifiesto, sin charset validado).
    #[must_use]
    pub fn new(rows: Vec<crate::palette::Row>) -> Self {
        let folds = rows
            .iter()
            .map(|row| crate::nav::fold(format!("{} {}", row.text, row.desc).as_bytes()))
            .collect();
        let mut p = Self {
            rows,
            folds,
            query: Vec::new(),
            visible: Vec::new(),
            cursor: 0,
            recent: Vec::new(),
        };
        p.recompute();
        p
    }

    /// ¿Es la fila `i`-ésima de [`Self::rows`] una de las recientes? Para
    /// que quien pinta pueda decirlo (un separador, un tono).
    #[must_use]
    pub fn is_recent(&self, i: usize) -> bool {
        self.rows
            .get(i)
            .is_some_and(|r| self.recent.contains(&r.key))
    }

    /// Añade filas a una palette YA abierta, conservando lo tecleado.
    ///
    /// Existe porque las filas de plugin no se pueden tener al abrir: salen
    /// de un `plugin.list` que hay que ir a pedir, y esperar a que conteste
    /// para pintar la palette es congelar la ventana por unas filas que
    /// puede que no haya. La alternativa —reconstruirla con `new`— pierde la
    /// query, que es justo lo que el lector acaba de teclear.
    ///
    /// El fold se calcula igual que en [`Self::new`]: sobre lo PINTADO
    /// (`text`+`desc`), jamás sobre `key`.
    pub fn extend_rows(&mut self, rows: Vec<crate::palette::Row>) {
        self.folds.extend(
            rows.iter()
                .map(|row| crate::nav::fold(format!("{} {}", row.text, row.desc).as_bytes())),
        );
        self.rows.extend(rows);
        self.recompute();
    }

    /// Recalcula `visible` a partir de la query actual sobre `self.folds`
    /// (el cache YA vigente) y clampa el cursor.
    fn recompute(&mut self) {
        self.visible = if self.query.is_empty() {
            // Las recientes primero, en su orden; luego el resto en el
            // orden de las filas.
            let mut out: Vec<usize> = self
                .recent
                .iter()
                .filter_map(|k| self.rows.iter().position(|r| r.key == *k))
                .collect();
            let rest: Vec<usize> = (0..self.rows.len()).filter(|i| !out.contains(i)).collect();
            out.extend(rest);
            out
        } else {
            let q = crate::nav::fold(&self.query);
            let exact: Vec<usize> = self
                .folds
                .iter()
                .enumerate()
                .filter(|(_, f)| f.contains(&q))
                .map(|(i, _)| i)
                .collect();
            if exact.is_empty() {
                // Sin substring, subsecuencia: `cpf` llega a «copy path».
                // Solo como RESPALDO, para que teclear lo que se ve siga
                // dando lo que se ve, y nada más, mientras case algo.
                self.folds
                    .iter()
                    .enumerate()
                    .filter(|(_, f)| is_subsequence(&q, f))
                    .map(|(i, _)| i)
                    .collect()
            } else {
                exact
            }
        };
        self.clamp_cursor();
    }

    fn clamp_cursor(&mut self) {
        if self.visible.is_empty() {
            self.cursor = 0;
        } else if self.cursor >= self.visible.len() {
            self.cursor = self.visible.len() - 1;
        }
    }

    /// Añade un carácter tecleado a la query y recalcula (mismo contrato que
    /// [`crate::nav::QuickSearch::push_char`]).
    pub fn push_char(&mut self, c: char) {
        let mut buf = [0u8; 4];
        self.query
            .extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
        self.recompute();
    }

    /// Retira el último char UTF-8 completo tecleado y recalcula.
    pub fn backspace(&mut self) {
        if self.query.is_empty() {
            return;
        }
        let mut cut = self.query.len() - 1;
        while cut > 0 && (self.query[cut] & 0b1100_0000) == 0b1000_0000 {
            cut -= 1;
        }
        self.query.truncate(cut);
        self.recompute();
    }

    /// Sube la selección (tope arriba).
    pub fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja la selección (tope al final).
    pub fn down(&mut self) {
        if self.cursor + 1 < self.visible.len() {
            self.cursor += 1;
        }
    }

    /// Sube `n` posiciones (pgup).
    pub fn page_up(&mut self, n: usize) {
        self.cursor = self.cursor.saturating_sub(n);
    }

    /// Baja `n` posiciones, tope al final (pgdn).
    pub fn page_down(&mut self, n: usize) {
        self.cursor = (self.cursor + n).min(self.visible.len().saturating_sub(1));
    }

    /// Índices REALES en `rows()` visibles con la query actual.
    #[must_use]
    pub fn visible(&self) -> &[usize] {
        &self.visible
    }

    /// Todas las filas ([`crate::palette::Row`]) — `rows()[visible()[i]]`
    /// para pintar la fila `i`-ésima de la lista filtrada. Solo `text`/
    /// `desc`/`chord` se pintan; `key` es de despacho interno (ver doc de
    /// [`crate::palette::Row`]).
    #[must_use]
    pub fn rows(&self) -> &[crate::palette::Row] {
        &self.rows
    }

    /// Posición de la selección DENTRO de `visible()` (para `ListState`).
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// La CLAVE de despacho bajo el cursor, si hay alguna visible (P1: ya no
    /// es `&'static str` — una fila de plugin trae una `key` construida en
    /// tiempo de ejecución, `plugin:{id}:{command}`; se clona porque
    /// `main::dispatch` la usa DESPUÉS de cerrar la palette, `app.palette =
    /// None`, que dropea `rows`).
    #[must_use]
    pub fn selected(&self) -> Option<String> {
        self.visible
            .get(self.cursor)
            .map(|&i| self.rows[i].key.clone())
    }

    /// Query para pintar (lossy, enmascarada — mismo contrato que
    /// [`crate::nav::QuickSearch::query_display`]: sin bracketed paste un
    /// paste hostil llega como stream de `push_char` y pintaría bidi/
    /// invisibles crudos en el borde).
    #[must_use]
    pub fn query_display(&self) -> String {
        String::from_utf8_lossy(&self.query)
            .chars()
            .map(|c| {
                if norte_encoding::is_terminal_hazard(c) {
                    '\u{FFFD}'
                } else {
                    c
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod palette_tests {
    use super::Palette;

    fn row(key: &str, desc: &str, chord: &str) -> crate::palette::Row {
        crate::palette::Row {
            key: key.to_owned(),
            text: key.to_owned(),
            desc: desc.to_owned(),
            chord: chord.to_owned(),
            hostile: false,
        }
    }

    fn rows() -> Vec<crate::palette::Row> {
        vec![
            row("app.quit", "quit norte", "q"),
            row("app.help", "this help", "f1"),
        ]
    }

    /// Recientes arriba con la consulta vacía, en su orden; una clave sin
    /// fila no pinta nada; y una consulta las devuelve al orden normal.
    #[test]
    fn palette_recientes_van_primero_solo_con_la_consulta_vacia() {
        let recent = vec!["app.help".to_owned(), "plugin:ya-no:existe".to_owned()];
        let mut p = Palette::with_recent(rows(), &recent);
        let visibles: Vec<&str> = p
            .visible()
            .iter()
            .map(|&i| p.rows()[i].key.as_str())
            .collect();
        assert_eq!(visibles, ["app.help", "app.quit"]);
        assert!(p.is_recent(p.visible()[0]) && !p.is_recent(p.visible()[1]));
        p.push_char('q');
        assert_eq!(p.selected().as_deref(), Some("app.quit"));
    }

    /// Sin substring, subsecuencia: `qn` casa «quit norte». Y mientras haya
    /// substring, la subsecuencia no mete ruido.
    #[test]
    fn palette_cae_a_subsecuencia_cuando_nada_casa_entero() {
        let mut p = Palette::new(rows());
        for c in "qn".chars() {
            p.push_char(c);
        }
        assert_eq!(p.selected().as_deref(), Some("app.quit"));
        let mut p = Palette::new(rows());
        for c in "help".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible().len(), 1, "substring exacto: solo app.help");
        assert!(super::is_subsequence("", "x") && !super::is_subsequence("ba", "ab"));
        assert!(
            !super::is_subsequence("\u{a9}", "é"),
            "por chars, no por bytes"
        );
    }

    #[test]
    fn palette_filtra_y_selecciona() {
        let mut p = Palette::new(rows());
        for c in "quit".chars() {
            p.push_char(c);
        }
        assert_eq!(p.visible().len(), 1, "solo app.quit casa con 'quit'");
        assert_eq!(p.selected().as_deref(), Some("app.quit"));
    }

    #[test]
    fn palette_query_hostil_se_enmascara() {
        let mut p = Palette::new(rows());
        for c in "a\u{202E}b".chars() {
            p.push_char(c);
        }
        let display = p.query_display();
        assert!(
            !display.chars().any(norte_encoding::is_terminal_hazard),
            "query_display dejó un hazard crudo: {display:?}"
        );
    }

    #[test]
    fn palette_filtro_vacio_muestra_todo() {
        let p = Palette::new(rows());
        assert_eq!(p.visible().len(), 2, "query vacía = todas las filas");
        assert_eq!(
            p.selected().as_deref(),
            Some("app.quit"),
            "cursor arranca en la primera"
        );
    }

    /// Filtro que NO casa con ninguna fila: `selected()` devuelve `None`
    /// (jamás un índice fantasma) y `up`/`down`/páginas no panican sobre
    /// `visible` vacío.
    #[test]
    fn palette_sin_matches_selected_es_none_y_no_panica() {
        let mut p = Palette::new(rows());
        for c in "zzz".chars() {
            p.push_char(c);
        }
        assert!(p.visible().is_empty());
        assert_eq!(p.selected(), None);
        p.up();
        p.down();
        p.page_up(3);
        p.page_down(3);
        assert_eq!(p.selected(), None);
    }

    /// (P1) Filas de plugin ([`crate::palette::plugin_rows`]) mezcladas con
    /// las built-in: el filtro de texto libre casa contra el TÍTULO YA
    /// enmascarado (`text`), y Enter (`selected()`) devuelve la `key` de
    /// despacho `plugin:{id}:{command}` — jamás el texto pintado.
    #[test]
    fn palette_filas_de_plugin_se_filtran_por_titulo_y_despachan_por_key() {
        let plugin = norte_proto::methods::PluginInfo {
            id: "org.norte.demo".into(),
            name: "Demo".into(),
            publisher: "norte".into(),
            version: "0.1.0".into(),
            category: "command".into(),
            capabilities: Vec::new(),
            approved: true,
            enabled: true,
            description: None,
            commands: vec![norte_proto::methods::PluginCommandInfo {
                id: "greet".into(),
                title: "Greet loudly".into(),
                kind: norte_proto::methods::PluginCommandKind::Command,
            }],
            columns: Vec::new(),
            has_help: false,
            manifest_digest: None,
        };
        let mut all = rows();
        all.extend(crate::palette::plugin_rows(&[plugin]));
        let mut p = Palette::new(all);
        for c in "loudly".chars() {
            p.push_char(c);
        }
        assert_eq!(
            p.visible().len(),
            1,
            "solo la fila de plugin casa con 'loudly' (el título)"
        );
        assert_eq!(p.selected().as_deref(), Some("plugin:org.norte.demo:greet"));
    }
}
