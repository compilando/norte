//! La rejilla de un terminal: los bytes que escribe un pty, parseados a
//! celdas, cursor y atributos.
//!
//! Es la mitad PURA de un panel de terminal, y la separación es la misma que
//! `norte-frontend`/`norte-tui` ya tienen para el subshell (ADR 0084): aquí no
//! hay pty, ni hilos, ni toolkit, ni tema. Entran bytes por
//! [`Pantalla::alimentar`] y sale una rejilla que cualquiera pinta — la
//! terminal con `ratatui`, la ventana como filas de spans por el puente.
//!
//! # Lo que este crate NO decide
//!
//! **El color de un índice.** Un terminal dice «color 1» y qué rojo sea eso es
//! del TEMA, no del terminal: [`ColorTerm::Indexado`] viaja tal cual y lo
//! resuelve quien pinta, con la paleta que el lector tenga puesta. Por eso
//! aquí no se depende de `norte-theme`: si la rejilla tradujera a RGB, el panel
//! dejaría de obedecer al tema y nadie podría arreglarlo desde el tema.
//!
//! **Lo que se enmascara.** Un programa dentro del panel es contenido AJENO, y
//! lo que pinta pasa por el mismo enmascarado que un nombre de fichero. Lo que
//! esta rejilla garantiza es más simple y es la base de eso: **en una celda no
//! puede acabar un byte de control**. Los escapes los come el parser y lo que
//! no entiende lo tira, así que un `\x1b[` a medias o un OSC entero no pintan
//! nada.
//!
//! ```
//! use norte_term::Pantalla;
//!
//! let mut p = Pantalla::nueva(10, 2);
//! p.alimentar(b"hola\r\nmundo");
//! assert_eq!(p.fila_texto(0).trim_end(), "hola");
//! assert_eq!(p.fila_texto(1).trim_end(), "mundo");
//! assert_eq!(p.cursor(), (1, 5));
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

#[cfg(feature = "pty")]
pub mod pty;

use unicode_width::UnicodeWidthChar as _;

/// Un color tal como lo DICE el terminal, sin resolver.
///
/// Los tres casos son los tres que existen en el wire de un terminal, y se
/// conservan distintos a propósito: un índice lo resuelve el tema de quien
/// pinta (ver el doc del crate), y un RGB ya venía decidido por el programa.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ColorTerm {
    /// Ninguno: el del fondo o el del texto normales de quien pinta.
    #[default]
    PorDefecto,
    /// Uno de los 256 de la paleta. El 0..=15 son los «de siempre».
    Indexado(u8),
    /// Uno exacto, que el programa eligió (`CSI 38;2;r;g;b m`).
    Rgb(u8, u8, u8),
}

/// Los atributos de una celda.
// Los seis son banderas SGR independientes, y el terminal las manda y las
// quita una a una (`SGR 1` / `SGR 22`). Un struct de bools ES esa
// representación; empaquetarlas en flags inventaría una forma que el wire de
// un terminal no tiene, y es el mismo criterio con el que `norte_theme::Style`
// guarda las suyas.
#[expect(
    clippy::struct_excessive_bools,
    reason = "seis atributos SGR independientes, no un enum ni flags empaquetadas"
)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Estilo {
    /// Color del texto.
    pub fg: ColorTerm,
    /// Color del fondo.
    pub bg: ColorTerm,
    /// `SGR 1`.
    pub negrita: bool,
    /// `SGR 2`.
    pub tenue: bool,
    /// `SGR 3`.
    pub cursiva: bool,
    /// `SGR 4`.
    pub subrayado: bool,
    /// `SGR 7`: los colores se cambian AL PINTAR, no aquí. Guardarlo resuelto
    /// perdería cuál era cuál, y `SGR 27` tiene que poder deshacerlo.
    pub inverso: bool,
    /// `SGR 9`.
    pub tachado: bool,
}

/// Una celda de la rejilla.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Celda {
    /// Lo que se ve. Un espacio si no se ha escrito nada.
    pub c: char,
    /// Cómo se ve.
    pub estilo: Estilo,
    /// Es la segunda mitad de un carácter ANCHO: no se pinta, y existe para
    /// que las columnas sigan cuadrando.
    pub estela: bool,
}

impl Default for Celda {
    fn default() -> Self {
        Self {
            c: ' ',
            estilo: Estilo::default(),
            estela: false,
        }
    }
}

/// La rejilla y el cursor: el estado, sin el parser.
///
/// Está separada de [`Pantalla`] por una razón mecánica y no de diseño:
/// `vte::Parser::advance` pide prestado el parser Y quien recibe los eventos,
/// y siendo el mismo objeto no se puede. Con dos campos, sí.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Rejilla {
    /// Ancho en columnas.
    ancho: u16,
    /// Alto en filas.
    alto: u16,
    /// `alto * ancho` celdas, por filas.
    celdas: Vec<Celda>,
    /// Fila del cursor.
    fila: u16,
    /// Columna del cursor. PUEDE valer `ancho`: es el estado «pendiente de
    /// salto» que un terminal de verdad tiene, y sin él escribir justo en el
    /// borde derecho saltaba de línea una celda antes de tiempo.
    col: u16,
    /// Lo oculta `CSI ?25l`.
    cursor_visible: bool,
    /// El estilo con el que se escribe ahora.
    estilo: Estilo,
    /// La REGIÓN de scroll, `[arriba, abajo]` inclusive (`DECSTBM`).
    ///
    /// Sin ella la pantalla entera sube cuando el cursor cae por abajo. Con
    /// ella sube SOLO ese trozo, que es lo que usa cualquier programa que
    /// fija una línea de estado: `tmux`, un `top`, un instalador con barra.
    /// Sin implementarla, la línea fijada se iba desplazando hacia arriba y
    /// acababa saliendo de la pantalla.
    region: (u16, u16),
    /// El cursor guardado por `ESC 7` / `CSI s`, y el estilo con él.
    ///
    /// El estilo va DENTRO porque `DECSC` lo guarda: restaurar la posición y
    /// dejar el color de otro sitio es lo que hace que un prompt salga a
    /// medias de un color.
    guardado: Option<(u16, u16, Estilo)>,
    /// El último carácter IMPRESO, para `REP` (`CSI b`).
    ultimo: Option<char>,
    /// ¿Está puesto el juego de dibujo de líneas (`ESC ( 0`)?
    dibujo: bool,
    /// La pantalla que `CSI ?1049h` apartó, si se apartó alguna.
    ///
    /// Es lo que hace que un `less` o un `vim` no dejen su último fotograma
    /// pegado al salir: entran, pintan sobre una pantalla limpia, y al salir
    /// se devuelve la de antes TAL CUAL —celdas, cursor y estilo—. De los dos
    /// modos, el moderno (`1049`) guarda además el cursor; el viejo (`47`) no,
    /// y esa diferencia se respeta.
    alterna: Option<Box<Guardada>>,
}

/// Una pantalla apartada por la pantalla alterna.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Guardada {
    celdas: Vec<Celda>,
    fila: u16,
    col: u16,
    estilo: Estilo,
    region: (u16, u16),
}

impl Rejilla {
    fn nueva(ancho: u16, alto: u16) -> Self {
        let (ancho, alto) = (ancho.max(1), alto.max(1));
        Self {
            ancho,
            alto,
            celdas: vec![Celda::default(); usize::from(ancho) * usize::from(alto)],
            fila: 0,
            col: 0,
            cursor_visible: true,
            estilo: Estilo::default(),
            region: (0, alto - 1),
            guardado: None,
            ultimo: None,
            dibujo: false,
            alterna: None,
        }
    }

    /// `DECSC`: la posición del cursor Y el estilo con el que se escribía.
    fn guardar_cursor(&mut self) {
        self.guardado = Some((self.fila, self.col, self.estilo));
    }

    /// `DECRC`. Sin nada guardado no hace nada, que es lo que dice la norma:
    /// inventarse la esquina superior izquierda movería el cursor de un
    /// programa que solo quería asegurarse.
    fn restaurar_cursor(&mut self) {
        if let Some((fila, col, estilo)) = self.guardado {
            self.fila = fila.min(self.alto - 1);
            self.col = col.min(self.ancho);
            self.estilo = estilo;
        }
    }

    /// Entra o sale de la pantalla alterna.
    ///
    /// `con_cursor` distingue el modo moderno (`1049`, guarda también dónde
    /// estaba el cursor) del viejo (`47`, solo la pantalla). Entrar dos veces
    /// no apila: la segunda no hace nada, o se perdería la pantalla de verdad.
    fn pantalla_alterna(&mut self, entrar: bool, con_cursor: bool) {
        if entrar {
            if self.alterna.is_some() {
                return;
            }
            self.alterna = Some(Box::new(Guardada {
                celdas: self.celdas.clone(),
                fila: self.fila,
                col: self.col,
                estilo: self.estilo,
                region: self.region,
            }));
            // La alterna empieza LIMPIA y sin región: es una pantalla nueva.
            self.celdas = vec![Celda::default(); self.celdas.len()];
            self.region = (0, self.alto - 1);
            if con_cursor {
                self.fila = 0;
                self.col = 0;
            }
            return;
        }
        let Some(g) = self.alterna.take() else {
            return;
        };
        self.celdas = g.celdas;
        self.region = g.region;
        if con_cursor {
            self.fila = g.fila.min(self.alto - 1);
            self.col = g.col.min(self.ancho);
            self.estilo = g.estilo;
        }
    }

    /// `ED`: borra la pantalla. `0` de aquí abajo, `1` de aquí arriba, y
    /// cualquier otra cosa, entera.
    fn borrar_pantalla(&mut self, modo: u16) {
        let (fila, col, alto, ancho) = (self.fila, self.col, self.alto, self.ancho);
        match modo {
            0 => {
                self.borrar_fila(fila, col, ancho);
                for f in fila + 1..alto {
                    self.borrar_fila(f, 0, ancho);
                }
            }
            1 => {
                for f in 0..fila {
                    self.borrar_fila(f, 0, ancho);
                }
                self.borrar_fila(fila, 0, col.saturating_add(1));
            }
            _ => {
                for f in 0..alto {
                    self.borrar_fila(f, 0, ancho);
                }
            }
        }
    }

    /// `EL`: lo mismo dentro de la fila del cursor.
    fn borrar_linea(&mut self, modo: u16) {
        let (fila, col, ancho) = (self.fila, self.col, self.ancho);
        match modo {
            0 => self.borrar_fila(fila, col, ancho),
            1 => self.borrar_fila(fila, 0, col.saturating_add(1)),
            _ => self.borrar_fila(fila, 0, ancho),
        }
    }

    /// `ICH`: abre `n` huecos en la fila del cursor, empujando a la derecha.
    fn insertar_celdas(&mut self, n: u16) {
        let (fila, col, ancho) = (self.fila, self.col, self.ancho);
        let n = n.min(ancho.saturating_sub(col));
        for c in (col..ancho).rev() {
            let origen = c.checked_sub(n).filter(|o| *o >= col);
            let celda = origen.and_then(|o| self.indice(fila, o).map(|i| self.celdas[i].clone()));
            if let Some(i) = self.indice(fila, c) {
                self.celdas[i] = celda.unwrap_or_default();
            }
        }
    }

    /// `DCH`: se lleva `n` celdas de la fila del cursor, tirando del resto.
    fn borrar_celdas(&mut self, n: u16) {
        let (fila, col, ancho) = (self.fila, self.col, self.ancho);
        let n = n.min(ancho.saturating_sub(col));
        for c in col..ancho {
            let origen = c.checked_add(n).filter(|o| *o < ancho);
            let celda = origen.and_then(|o| self.indice(fila, o).map(|i| self.celdas[i].clone()));
            if let Some(i) = self.indice(fila, c) {
                self.celdas[i] = celda.unwrap_or_default();
            }
        }
    }

    fn indice(&self, fila: u16, col: u16) -> Option<usize> {
        (fila < self.alto && col < self.ancho)
            .then(|| usize::from(fila) * usize::from(self.ancho) + usize::from(col))
    }

    /// Baja una fila, y si ya estaba en el BORDE DE ABAJO DE LA REGIÓN sube su
    /// contenido.
    ///
    /// Sin esto, lo último que escribe un shell es justo lo que no se ve. Y sin
    /// mirar la región, un programa con una línea de estado fijada veía cómo
    /// esa línea se iba hacia arriba hasta desaparecer.
    ///
    /// Fuera de la región el cursor baja y ya: ahí no hay scroll, que es
    /// exactamente lo que la región promete.
    fn bajar(&mut self) {
        let (arriba, abajo) = self.region;
        if self.fila == abajo {
            self.subir_region(arriba, abajo, 1);
            return;
        }
        if self.fila + 1 < self.alto {
            self.fila += 1;
        }
    }

    /// Sube `n` filas el trozo `[arriba, abajo]`, rellenando por abajo.
    fn subir_region(&mut self, arriba: u16, abajo: u16, n: u16) {
        if arriba > abajo {
            return;
        }
        let alto_region = abajo - arriba + 1;
        let n = n.min(alto_region);
        for f in arriba..=abajo {
            let origen = f + n;
            for c in 0..self.ancho {
                let celda = if origen <= abajo {
                    self.indice(origen, c).map(|i| self.celdas[i].clone())
                } else {
                    None
                };
                if let Some(i) = self.indice(f, c) {
                    self.celdas[i] = celda.unwrap_or_default();
                }
            }
        }
    }

    /// Baja `n` filas el trozo `[arriba, abajo]`, rellenando por arriba. Es la
    /// inversa de [`Self::subir_region`], y la usa `IL`.
    fn bajar_region(&mut self, arriba: u16, abajo: u16, n: u16) {
        if arriba > abajo {
            return;
        }
        let alto_region = abajo - arriba + 1;
        let n = n.min(alto_region);
        for f in (arriba..=abajo).rev() {
            let origen = f.checked_sub(n).filter(|o| *o >= arriba);
            for c in 0..self.ancho {
                let celda = origen.and_then(|o| self.indice(o, c).map(|i| self.celdas[i].clone()));
                if let Some(i) = self.indice(f, c) {
                    self.celdas[i] = celda.unwrap_or_default();
                }
            }
        }
    }

    /// El carácter que `ESC ( 0` pinta en lugar de `c` (DEC Special Graphics).
    ///
    /// Solo el tramo `0x60..=0x7e`, que es el que el juego redefine; el resto
    /// se queda como está. Sin esto, un programa que dibuja una caja imprimía
    /// `lqqqk` donde quería una esquina y tres rayas.
    fn dibujar(c: char) -> char {
        const TABLA: &str = "◆▒␉␌␍␊°±␤␋┘┐┌└┼⎺⎻─⎼⎽├┤┴┬│≤≥π≠£·";
        let i = (c as u32)
            .checked_sub(0x60)
            .and_then(|i| usize::try_from(i).ok());
        i.and_then(|i| TABLA.chars().nth(i)).unwrap_or(c)
    }

    /// Escribe un carácter donde esté el cursor y lo adelanta.
    fn poner(&mut self, c: char) {
        let c = if self.dibujo { Self::dibujar(c) } else { c };
        // Para `REP`, que repite el último IMPRESO — incluido el traducido:
        // lo que se repite es lo que se ve.
        self.ultimo = Some(c);
        // Un carácter de anchura cero —una combinante— no tiene celda propia.
        // Se DESCARTA, y eso es una limitación conocida: lo correcto es
        // pegarlo al carácter anterior, y eso pide que una celda guarde un
        // clúster y no un `char`. Hasta entonces, descartar es lo que no
        // descoloca las columnas.
        let ancho_c = u16::try_from(c.width().unwrap_or(0)).unwrap_or(0);
        if ancho_c == 0 {
            return;
        }
        // Saturante como los movimientos de cursor, y por el mismo motivo: la
        // columna sale de un parámetro ajeno. Aquí haría falta una rejilla de
        // 65528 columnas para desbordar —o sea, nunca—, pero la clase entera
        // se cierra de una vez en vez de dejar cuatro sitios que hay que
        // volver a razonar cada vez que alguien los lee.
        if self.col.saturating_add(ancho_c) > self.ancho {
            self.col = 0;
            self.bajar();
        }
        let (fila, col, estilo) = (self.fila, self.col, self.estilo);
        if let Some(i) = self.indice(fila, col) {
            self.celdas[i] = Celda {
                c,
                estilo,
                estela: false,
            };
        }
        for d in 1..ancho_c {
            if let Some(i) = self.indice(fila, col + d) {
                self.celdas[i] = Celda {
                    c: ' ',
                    estilo,
                    estela: true,
                };
            }
        }
        // Acotado a `ancho`, y no un `+=` suelto: un carácter ANCHO en una
        // rejilla de UNA columna no cabe ni después de envolver —el salto de
        // arriba deja `col` en 0 y sigue sin caber— así que sumarle 2 dejaba
        // el cursor en la columna 2 de una rejilla que llega hasta la 1. Lo
        // encontró la proptest, que es para lo que está: una rejilla de una
        // columna no se le ocurre a nadie y un pty la produce en cuanto la
        // ventana se estrecha.
        //
        // `ancho` y no `ancho - 1` porque ésa es la posición de envoltura
        // pendiente, que es un estado legítimo del cursor aquí.
        self.col = self.col.saturating_add(ancho_c).min(self.ancho);
    }

    /// Deja `rango` de celdas de la fila `fila` como recién puestas.
    fn borrar_fila(&mut self, fila: u16, desde: u16, hasta: u16) {
        for col in desde..hasta.min(self.ancho) {
            if let Some(i) = self.indice(fila, col) {
                self.celdas[i] = Celda::default();
            }
        }
    }

    /// `SGR`: los atributos, con los dos que llevan argumentos dentro.
    fn sgr(&mut self, codigos: &[u16]) {
        // Un `CSI m` pelado es un `CSI 0 m`: limpiar.
        if codigos.is_empty() {
            self.estilo = Estilo::default();
            return;
        }
        let mut i = 0;
        while i < codigos.len() {
            let n = codigos[i];
            match n {
                0 => self.estilo = Estilo::default(),
                1 => self.estilo.negrita = true,
                2 => self.estilo.tenue = true,
                3 => self.estilo.cursiva = true,
                4 => self.estilo.subrayado = true,
                7 => self.estilo.inverso = true,
                9 => self.estilo.tachado = true,
                // El 21 es «doble subrayado» en unos terminales y «quitar la
                // negrita» en otros; se trata como el 22, que es lo que hacen
                // los que importan.
                21 | 22 => {
                    self.estilo.negrita = false;
                    self.estilo.tenue = false;
                }
                23 => self.estilo.cursiva = false,
                24 => self.estilo.subrayado = false,
                27 => self.estilo.inverso = false,
                29 => self.estilo.tachado = false,
                30..=37 => self.estilo.fg = ColorTerm::Indexado(u8_de(n - 30)),
                39 => self.estilo.fg = ColorTerm::PorDefecto,
                40..=47 => self.estilo.bg = ColorTerm::Indexado(u8_de(n - 40)),
                49 => self.estilo.bg = ColorTerm::PorDefecto,
                // Los «brillantes» son los índices 8..=15 de la misma paleta,
                // no otra cosa: resolverlos aquí a un RGB le quitaría al tema
                // la posibilidad de decir cómo son.
                90..=97 => self.estilo.fg = ColorTerm::Indexado(u8_de(n - 90 + 8)),
                100..=107 => self.estilo.bg = ColorTerm::Indexado(u8_de(n - 100 + 8)),
                38 | 48 => {
                    let (color, leidos) = color_extendido(&codigos[i + 1..]);
                    if let Some(color) = color {
                        if n == 38 {
                            self.estilo.fg = color;
                        } else {
                            self.estilo.bg = color;
                        }
                    }
                    i += leidos;
                }
                _ => {}
            }
            i += 1;
        }
    }
}

/// Un `u16` de un código SGR que YA se sabe pequeño, sin `unwrap`.
fn u8_de(n: u16) -> u8 {
    u8::try_from(n).unwrap_or(0)
}

/// Lee lo que va detrás de un `38`/`48`: `5;n` (paleta) o `2;r;g;b` (exacto).
///
/// Devuelve el color y CUÁNTOS códigos se ha comido, para que quien llama siga
/// por donde toca. Un `38` mal formado se traga sus argumentos igual: dejarlos
/// pasar los interpretaría como atributos sueltos y pintaría de cualquier cosa.
fn color_extendido(resto: &[u16]) -> (Option<ColorTerm>, usize) {
    match resto.first() {
        Some(5) => (
            resto.get(1).map(|n| ColorTerm::Indexado(u8_de(*n))),
            resto.len().min(2),
        ),
        Some(2) => (
            match (resto.get(1), resto.get(2), resto.get(3)) {
                (Some(r), Some(g), Some(b)) => {
                    Some(ColorTerm::Rgb(u8_de(*r), u8_de(*g), u8_de(*b)))
                }
                _ => None,
            },
            resto.len().min(4),
        ),
        _ => (None, 0),
    }
}

impl vte::Perform for Rejilla {
    fn print(&mut self, c: char) {
        self.poner(c);
    }

    /// Los C0 que mueven el cursor. **Todo lo demás se tira**, y eso es la
    /// garantía del crate: un byte de control no puede acabar en una celda.
    fn execute(&mut self, byte: u8) {
        match byte {
            b'\n' | 0x0b | 0x0c => self.bajar(),
            b'\r' => self.col = 0,
            0x08 => self.col = self.col.saturating_sub(1),
            b'\t' => self.col = (self.col / 8).saturating_add(1).saturating_mul(8),
            _ => return,
        }
        self.col = self.col.min(self.ancho);
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermedios: &[u8],
        _ignora: bool,
        accion: char,
    ) {
        // Los subparámetros (`38:2:…`) se aplanan con los parámetros
        // (`38;2;…`): son dos formas de escribir lo mismo y ningún programa
        // debería notar cuál usó.
        let codigos: Vec<u16> = params.iter().flat_map(|p| p.iter().copied()).collect();
        // Un parámetro ausente vale 1 para los movimientos y 0 para los
        // borrados, que es lo que dice ECMA-48 y lo que un `tput` espera.
        let uno = |n: usize| codigos.get(n).copied().filter(|v| *v != 0).unwrap_or(1);
        let cero = |n: usize| codigos.get(n).copied().unwrap_or(0);
        // `?` llega como marcador privado, y sin mirarlo un `CSI 25 l` normal
        // escondería el cursor sin haberlo pedido.
        let privado = intermedios.first() == Some(&b'?');
        match accion {
            'm' if !privado => self.sgr(&codigos),
            // `CSI H` cuenta desde UNO; la rejilla, desde cero.
            'H' | 'f' if !privado => {
                self.fila = (uno(0) - 1).min(self.alto - 1);
                self.col = (uno(1) - 1).min(self.ancho - 1);
            }
            // Los cuatro SATURAN, y los cuatro tienen que hacerlo: `uno` viene
            // de un parámetro que lo escribió otro programa y puede valer
            // 65535. Sumarlo desbordaba el `u16` y, con comprobaciones puestas
            // —el perfil con el que corre la suite y el binario de desarrollo—,
            // eso es un pánico que dispara cualquier fichero con `ESC [ 6 5 5
            // 3 5 C` dentro. Que `A` y `D` fueran saturantes y `B` y `C` no era
            // el síntoma a la vista.
            'A' => self.fila = self.fila.saturating_sub(uno(0)),
            'B' => self.fila = self.fila.saturating_add(uno(0)).min(self.alto - 1),
            'C' => self.col = self.col.saturating_add(uno(0)).min(self.ancho - 1),
            'D' => self.col = self.col.saturating_sub(uno(0)),
            'J' if !privado => self.borrar_pantalla(cero(0)),
            'K' if !privado => self.borrar_linea(cero(0)),
            // Posición absoluta en UN eje: `CHA` la columna, `VPA` la fila.
            // Baratas y constantes — un prompt que se repinta las usa en cada
            // pulsación—, y sin ellas se quedaba escribiendo donde estuviera.
            'G' | '`' if !privado => self.col = (uno(0) - 1).min(self.ancho - 1),
            'd' if !privado => self.fila = (uno(0) - 1).min(self.alto - 1),
            // Insertar y borrar LÍNEAS, dentro de la región y desde el cursor:
            // es lo que usa un editor para abrir hueco sin repintar el resto.
            'L' if !privado => {
                let (arriba, abajo) = self.region;
                if self.fila >= arriba && self.fila <= abajo {
                    self.bajar_region(self.fila, abajo, uno(0));
                }
            }
            'M' if !privado => {
                let (arriba, abajo) = self.region;
                if self.fila >= arriba && self.fila <= abajo {
                    self.subir_region(self.fila, abajo, uno(0));
                }
            }
            // Insertar y borrar CARACTERES en la fila del cursor, y borrar sin
            // mover: lo que un editor de línea usa para no repintar la línea
            // entera en cada tecla.
            '@' if !privado => self.insertar_celdas(uno(0)),
            'P' if !privado => self.borrar_celdas(uno(0)),
            'X' if !privado => {
                let (fila, col) = (self.fila, self.col);
                self.borrar_fila(fila, col, col.saturating_add(uno(0)));
            }
            // Subir y bajar la región sin mover el cursor (`SU`/`SD`).
            'S' if !privado => {
                let (arriba, abajo) = self.region;
                self.subir_region(arriba, abajo, uno(0));
            }
            'T' if !privado => {
                let (arriba, abajo) = self.region;
                self.bajar_region(arriba, abajo, uno(0));
            }
            // `REP`: repetir el último carácter impreso. Un `tput rep` lo usa
            // para pintar una línea de guiones con cuatro bytes.
            'b' if !privado => {
                if let Some(c) = self.ultimo {
                    for _ in 0..uno(0) {
                        self.poner(c);
                    }
                }
            }
            // `DECSTBM`: la región de scroll. Sin parámetros vuelve a ser la
            // pantalla entera, y el cursor va a su esquina — eso lo dice la
            // norma y lo dan por hecho los programas que la fijan.
            'r' if !privado => {
                let arriba = codigos.first().copied().filter(|v| *v != 0).unwrap_or(1) - 1;
                let abajo = codigos
                    .get(1)
                    .copied()
                    .filter(|v| *v != 0)
                    .unwrap_or(self.alto)
                    - 1;
                let (arriba, abajo) = (arriba.min(self.alto - 1), abajo.min(self.alto - 1));
                // Una región al revés o de una sola fila no se acepta: no hay
                // nada que desplazar y dejarla puesta rompe el scroll normal.
                if arriba < abajo {
                    self.region = (arriba, abajo);
                    self.fila = arriba;
                    self.col = 0;
                }
            }
            // `SCP`/`RCP`, los gemelos de `ESC 7`/`ESC 8` en forma de CSI.
            's' if !privado => self.guardar_cursor(),
            'u' if !privado => self.restaurar_cursor(),
            'h' | 'l' if privado => {
                let encender = accion == 'h';
                match codigos.first() {
                    Some(&25) => self.cursor_visible = encender,
                    // La pantalla ALTERNA. `1049` es la moderna —aparta
                    // pantalla Y cursor— y `47`/`1047` la vieja, que solo
                    // aparta la pantalla. Es la que hace que un `less` no deje
                    // su último fotograma pegado al salir.
                    Some(&1049) => self.pantalla_alterna(encender, true),
                    Some(&47 | &1047) => self.pantalla_alterna(encender, false),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn esc_dispatch(&mut self, intermedios: &[u8], _ignora: bool, byte: u8) {
        match (intermedios.first(), byte) {
            // `DECSC`/`DECRC`: guardar y restaurar el cursor.
            (None, b'7') => self.guardar_cursor(),
            (None, b'8') => self.restaurar_cursor(),
            // `RIS`: reinicio completo. Lo manda un `reset`, y también un
            // programa que se encontró la pantalla en un estado que no
            // entiende — que es justo cuando hay que obedecer.
            (None, b'c') => {
                let (ancho, alto) = (self.ancho, self.alto);
                *self = Self::nueva(ancho, alto);
            }
            // `ESC ( 0` y `ESC ( B`: el juego de caracteres de dibujo de
            // líneas y la vuelta a ASCII. Ignorarlo hacía que un programa que
            // dibuja cajas imprimiera `lqqqk` donde quería esquinas.
            (Some(b'('), b'0') => self.dibujo = true,
            (Some(b'('), b'B') => self.dibujo = false,
            // `IND` y `NEL` bajan una línea (con scroll de región); `RI` sube.
            (None, b'D') => self.bajar(),
            (None, b'E') => {
                self.bajar();
                self.col = 0;
            }
            (None, b'M') => {
                let (arriba, abajo) = self.region;
                if self.fila == arriba {
                    self.bajar_region(arriba, abajo, 1);
                } else {
                    self.fila = self.fila.saturating_sub(1);
                }
            }
            _ => {}
        }
    }
}

/// La pantalla de un terminal: la rejilla y el parser de escapes.
pub struct Pantalla {
    /// El estado.
    rejilla: Rejilla,
    /// La máquina de estados de los escapes.
    parser: vte::Parser,
}

impl Pantalla {
    /// Una pantalla vacía de `ancho` por `alto`.
    ///
    /// Un tamaño de cero en cualquier eje se sube a uno: una rejilla sin celdas
    /// no tiene dónde poner el cursor, y quien pinta un panel de cero columnas
    /// no quiere un `None` en cada celda, quiere que no se caiga.
    #[must_use]
    pub fn nueva(ancho: u16, alto: u16) -> Self {
        Self {
            rejilla: Rejilla::nueva(ancho, alto),
            parser: vte::Parser::new(),
        }
    }

    /// El tamaño, en columnas y filas.
    #[must_use]
    pub fn tamano(&self) -> (u16, u16) {
        (self.rejilla.ancho, self.rejilla.alto)
    }

    /// Dónde está el cursor: fila y columna.
    ///
    /// La columna puede valer tanto como el ancho, y eso no es un error: es el
    /// terminal esperando a ver si lo siguiente que llega necesita saltar.
    #[must_use]
    pub fn cursor(&self) -> (u16, u16) {
        (self.rejilla.fila, self.rejilla.col)
    }

    /// ¿Se pinta el cursor? (`CSI ?25l` / `CSI ?25h`).
    #[must_use]
    pub fn cursor_visible(&self) -> bool {
        self.rejilla.cursor_visible
    }

    /// La celda de `fila`, `col`, o `None` si cae fuera.
    #[must_use]
    pub fn celda(&self, fila: u16, col: u16) -> Option<&Celda> {
        self.rejilla
            .indice(fila, col)
            .and_then(|i| self.rejilla.celdas.get(i))
    }

    /// Una fila partida en TRAMOS: texto seguido que comparte estilo.
    ///
    /// Es lo que cualquiera que pinte necesita, y por eso vive aquí y no en
    /// cada frontend: la terminal hace un span de `ratatui` por tramo y la
    /// ventana un `TerminalSpanView`, pero *dónde* se corta es la misma
    /// decisión y se toma una vez.
    ///
    /// Agrupar no es cosmético. Una fila de ochenta celdas son ochenta
    /// fragmentos si no se agrupa, y eso se paga en cada repintado — por el
    /// puente, además, donde son ochenta objetos JSON.
    ///
    /// Las estelas de un carácter ancho no salen: ya están dentro del tramo de
    /// su carácter.
    ///
    /// ```
    /// use norte_term::Pantalla;
    ///
    /// let mut p = Pantalla::nueva(8, 1);
    /// p.alimentar(b"ab\x1b[31mcd");
    /// let tramos = p.fila_tramos(0);
    /// // Tres: lo normal, lo rojo, y el relleno del final —que NO es rojo, y
    /// // meterlo en el tramo de antes pintaría de rojo el resto de la línea.
    /// assert_eq!(tramos.len(), 3);
    /// assert_eq!(tramos[0].0, "ab");
    /// assert_eq!(tramos[1].0, "cd");
    /// assert_eq!(tramos[2].0, "    ");
    /// ```
    #[must_use]
    pub fn fila_tramos(&self, fila: u16) -> Vec<(String, Estilo)> {
        let mut tramos: Vec<(String, Estilo)> = Vec::new();
        for c in (0..self.rejilla.ancho).filter_map(|c| self.celda(fila, c)) {
            if c.estela {
                continue;
            }
            match tramos.last_mut() {
                Some((texto, estilo)) if *estilo == c.estilo => texto.push(c.c),
                _ => tramos.push((c.c.to_string(), c.estilo)),
            }
        }
        tramos
    }

    /// El texto de una fila, con las estelas quitadas.
    ///
    /// Existe para los tests y para quien quiera una línea de un tirón; lo que
    /// pinta con estilos itera [`Self::celda`] o [`Self::fila_tramos`].
    #[must_use]
    pub fn fila_texto(&self, fila: u16) -> String {
        (0..self.rejilla.ancho)
            .filter_map(|c| self.celda(fila, c))
            .filter(|c| !c.estela)
            .map(|c| c.c)
            .collect()
    }

    /// Le da bytes del pty.
    ///
    /// Se puede llamar con los trozos que devuelva una lectura, partidos por
    /// donde sea: el parser guarda lo que lleve a medias, que es lo normal
    /// cuando un escape cae en la frontera de dos lecturas.
    pub fn alimentar(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.rejilla, bytes);
    }

    /// Cambia el tamaño, conservando lo que cabe.
    ///
    /// No se re-ajusta el texto a lo ancho: lo que sobra por la derecha se
    /// pierde y lo que sobra por abajo también. Reajustar pide saber qué
    /// líneas eran una sola partida en dos, y esta rejilla no lo guarda —
    /// tampoco lo guardan la mitad de los terminales que la gente usa.
    pub fn redimensionar(&mut self, ancho: u16, alto: u16) {
        let (ancho, alto) = (ancho.max(1), alto.max(1));
        let vieja = std::mem::replace(&mut self.rejilla, Rejilla::nueva(ancho, alto));
        for fila in 0..alto.min(vieja.alto) {
            for col in 0..ancho.min(vieja.ancho) {
                if let (Some(destino), Some(origen)) =
                    (self.rejilla.indice(fila, col), vieja.indice(fila, col))
                {
                    self.rejilla.celdas[destino] = vieja.celdas[origen].clone();
                }
            }
        }
        self.rejilla.estilo = vieja.estilo;
        self.rejilla.cursor_visible = vieja.cursor_visible;
        self.rejilla.fila = vieja.fila.min(alto - 1);
        self.rejilla.col = vieja.col.min(ancho);
        self.rejilla.dibujo = vieja.dibujo;
        self.rejilla.ultimo = vieja.ultimo;
        // El cursor guardado y la región se ACOTAN al tamaño nuevo en vez de
        // tirarse: un `vim` que redimensiona mientras tiene su región puesta
        // no la vuelve a mandar, y perderla le desharía la línea de estado.
        // Una región que ya no cabe vuelve a ser la pantalla entera, que es lo
        // que un terminal de verdad hace.
        self.rejilla.guardado = vieja
            .guardado
            .map(|(f, c, e)| (f.min(alto - 1), c.min(ancho), e));
        let (arriba, abajo) = vieja.region;
        self.rejilla.region = if arriba < abajo.min(alto - 1) {
            (arriba.min(alto - 1), abajo.min(alto - 1))
        } else {
            (0, alto - 1)
        };
        // Y la pantalla apartada se conserva RECORTADA: perderla dejaría a un
        // `less` sin nada que devolver al salir, que es el fallo que la
        // pantalla alterna existe para no tener.
        self.rejilla.alterna = vieja.alterna.map(|g| {
            let mut celdas = vec![Celda::default(); usize::from(ancho) * usize::from(alto)];
            for fila in 0..alto.min(vieja.alto) {
                for col in 0..ancho.min(vieja.ancho) {
                    let destino = usize::from(fila) * usize::from(ancho) + usize::from(col);
                    let origen = usize::from(fila) * usize::from(vieja.ancho) + usize::from(col);
                    if let Some(c) = g.celdas.get(origen) {
                        celdas[destino] = c.clone();
                    }
                }
            }
            Box::new(Guardada {
                celdas,
                fila: g.fila.min(alto - 1),
                col: g.col.min(ancho),
                estilo: g.estilo,
                region: (0, alto - 1),
            })
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Las filas con texto, sin los rellenos de la derecha: es lo que un test
    /// quiere comparar y escribirlo a mano en cada uno era la mitad del ruido.
    fn pantalla(p: &Pantalla, alto: u16) -> Vec<String> {
        (0..alto)
            .map(|f| p.fila_texto(f).trim_end().to_owned())
            .collect()
    }

    /// **La pantalla ALTERNA devuelve exactamente lo que había** (#366).
    ///
    /// Es el punto entero de `CSI ?1049h`: un `less` o un `vim` entran, pintan
    /// sobre una pantalla limpia y al salir la de antes vuelve TAL CUAL. Sin
    /// esto, lo que quedaba pegado en el panel era el último fotograma del
    /// programa que acababa de salir — el fallo que se ve como roto y no como
    /// pobre.
    #[test]
    fn la_pantalla_alterna_devuelve_lo_que_habia() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"uno\r\ndos\r\ntres");
        let antes = pantalla(&p, 3);
        let cursor_antes = p.cursor();

        // Entra un programa de pantalla completa y pinta lo suyo.
        p.alimentar(b"\x1b[?1049h");
        assert_eq!(
            pantalla(&p, 3),
            vec![String::new(), String::new(), String::new()],
            "la alterna empieza limpia"
        );
        assert_eq!(p.cursor(), (0, 0), "y con el cursor en la esquina");
        p.alimentar(b"visor");
        assert_eq!(p.fila_texto(0).trim_end(), "visor");

        // Y sale.
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(pantalla(&p, 3), antes, "no volvió lo que había");
        assert_eq!(p.cursor(), cursor_antes, "ni el cursor");
    }

    /// **Redimensionar con la alterna puesta no pierde la pantalla de abajo.**
    ///
    /// Es fácil de olvidar porque el cambio de tamaño construye una rejilla
    /// nueva: si lo apartado no viaja con ella, un `less` al que el lector
    /// cambia el tamaño de la ventana se queda sin nada que devolver al salir
    /// — justo el fallo que la pantalla alterna existe para no tener.
    #[test]
    fn redimensionar_con_la_alterna_puesta_no_pierde_lo_de_abajo() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"real");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"visor");
        p.redimensionar(12, 4);
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(p.fila_texto(0).trim_end(), "real");
    }

    /// Entrar dos veces en la alterna no apila: la segunda no hace nada. Si
    /// apilara, la pantalla de VERDAD se perdería al salir una sola vez.
    #[test]
    fn entrar_dos_veces_en_la_alterna_no_pierde_la_de_verdad() {
        let mut p = Pantalla::nueva(10, 2);
        p.alimentar(b"real");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"primera");
        p.alimentar(b"\x1b[?1049h");
        p.alimentar(b"\x1b[?1049l");
        assert_eq!(p.fila_texto(0).trim_end(), "real");
    }

    /// **La región de scroll deja quieto lo que hay fuera** (#366).
    ///
    /// Lo usa todo lo que fija una línea de estado. Sin ella, esa línea se iba
    /// desplazando hacia arriba con cada salto hasta salirse de la pantalla.
    #[test]
    fn la_region_de_scroll_no_mueve_lo_de_fuera() {
        let mut p = Pantalla::nueva(10, 4);
        // Una cabecera fija arriba y la región en las tres de abajo.
        p.alimentar(b"cabecera\x1b[2;4r");
        // `DECSTBM` deja el cursor en la esquina de la región.
        assert_eq!(p.cursor(), (1, 0));
        p.alimentar(b"a\r\nb\r\nc\r\nd");
        assert_eq!(
            pantalla(&p, 4),
            vec![
                "cabecera".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                "d".to_owned()
            ],
            "la cabecera se movió, o la región no desplazó"
        );
    }

    /// `CHA` y `VPA`: posición absoluta en un solo eje. Las usa cualquier
    /// prompt que se repinta, y sin ellas escribía donde estuviera.
    #[test]
    fn la_posicion_absoluta_de_un_solo_eje() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"abcdef\x1b[3Gx");
        assert_eq!(p.fila_texto(0).trim_end(), "abxdef");
        // `VPA` mueve la FILA y deja la columna donde estaba: ésa es la mitad
        // que la distingue de un `CUP`, y la que un prompt aprovecha.
        assert_eq!(p.cursor(), (0, 3));
        p.alimentar(b"\x1b[3dy");
        assert_eq!(p.cursor().0, 2, "VPA no llevó a la fila 3");
        assert_eq!(p.celda(2, 3).map(|c| c.c), Some('y'));
    }

    /// Insertar y borrar caracteres en la fila: lo que un editor de línea usa
    /// para no repintar la línea entera en cada tecla.
    #[test]
    fn insertar_y_borrar_caracteres_en_la_fila() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"abcdef\x1b[1G\x1b[2@");
        assert_eq!(p.fila_texto(0).trim_end(), "  abcdef");
        p.alimentar(b"\x1b[1G\x1b[3P");
        assert_eq!(p.fila_texto(0).trim_end(), "bcdef");
        // `ECH` borra SIN mover ni tirar del resto.
        p.alimentar(b"\x1b[1G\x1b[2X");
        assert_eq!(p.fila_texto(0).trim_end(), "  def");
    }

    /// Insertar y borrar LÍNEAS, dentro de la región.
    #[test]
    fn insertar_y_borrar_lineas() {
        let mut p = Pantalla::nueva(10, 4);
        p.alimentar(b"a\r\nb\r\nc\r\nd\x1b[2;1H\x1b[L");
        assert_eq!(
            pantalla(&p, 4),
            vec![
                "a".to_owned(),
                String::new(),
                "b".to_owned(),
                "c".to_owned()
            ],
        );
        p.alimentar(b"\x1b[2;1H\x1b[M");
        assert_eq!(
            pantalla(&p, 4),
            vec![
                "a".to_owned(),
                "b".to_owned(),
                "c".to_owned(),
                String::new()
            ],
        );
    }

    /// Guardar y restaurar el cursor, con su ESTILO: restaurar la posición y
    /// dejar el color de otro sitio es lo que saca un prompt a medio pintar.
    #[test]
    fn guardar_y_restaurar_el_cursor_lleva_el_estilo() {
        let mut p = Pantalla::nueva(10, 2);
        p.alimentar(b"\x1b[31m\x1b[1;3H\x1b7");
        p.alimentar(b"\x1b[0m\x1b[2;1Hxx\x1b8y");
        assert_eq!(p.cursor(), (0, 3), "el cursor no volvió a donde se guardó");
        let c = p.celda(0, 2).expect("dentro");
        assert_eq!(c.c, 'y');
        assert_eq!(
            c.estilo.fg,
            ColorTerm::Indexado(1),
            "restauró la posición y no el estilo"
        );
    }

    /// `RIS` deja la pantalla como recién abierta: es lo que manda un `reset`,
    /// y lo manda quien se encontró la pantalla en un estado que no entiende.
    #[test]
    fn ris_reinicia_del_todo() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"\x1b[31mhola\x1b[2;3r\x1b[?25l");
        p.alimentar(b"\x1bc");
        assert_eq!(
            pantalla(&p, 3),
            vec![String::new(), String::new(), String::new()]
        );
        assert_eq!(p.cursor(), (0, 0));
        assert!(p.cursor_visible(), "el cursor siguió escondido");
    }

    /// `REP` repite el último carácter impreso: un `tput rep` pinta una línea
    /// de guiones con cuatro bytes en vez de con ochenta.
    #[test]
    fn rep_repite_el_ultimo() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"-\x1b[4b");
        assert_eq!(p.fila_texto(0).trim_end(), "-----");
    }

    /// El juego de DIBUJO: sin él, un programa que dibuja cajas imprimía
    /// `lqqqk` donde quería una esquina y tres rayas.
    #[test]
    fn el_juego_de_dibujo_pinta_lineas_y_no_letras() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"\x1b(0lqqk\x1b(Bx");
        assert_eq!(p.fila_texto(0).trim_end(), "┌──┐x");
    }

    #[test]
    fn el_texto_llena_la_fila_y_el_salto_baja() {
        let mut p = Pantalla::nueva(8, 3);
        p.alimentar(b"uno\r\ndos");
        assert_eq!(p.fila_texto(0).trim_end(), "uno");
        assert_eq!(p.fila_texto(1).trim_end(), "dos");
        assert_eq!(p.cursor(), (1, 3));
    }

    /// El borde derecho no salta ANTES de tiempo: la última columna se puede
    /// escribir, y el salto ocurre con el carácter siguiente. Sin el estado
    /// «pendiente», una palabra de ocho letras en ocho columnas dejaba la
    /// última en la fila de abajo.
    #[test]
    fn el_borde_derecho_salta_despues_y_no_antes() {
        let mut p = Pantalla::nueva(4, 2);
        p.alimentar(b"abcd");
        assert_eq!(p.fila_texto(0), "abcd");
        assert_eq!(p.cursor(), (0, 4), "pendiente de salto, no saltado");
        p.alimentar(b"e");
        assert_eq!(p.fila_texto(1).trim_end(), "e");
        assert_eq!(p.cursor(), (1, 1));
    }

    /// Al llegar al fondo, la pantalla SUBE: sin esto lo último que escribe un
    /// shell es lo que no se ve.
    #[test]
    fn el_fondo_desplaza_hacia_arriba() {
        let mut p = Pantalla::nueva(4, 2);
        p.alimentar(b"a\r\nb\r\nc");
        assert_eq!(p.fila_texto(0).trim_end(), "b");
        assert_eq!(p.fila_texto(1).trim_end(), "c");
        assert_eq!(p.cursor(), (1, 1));
    }

    #[test]
    fn los_atributos_se_ponen_y_se_quitan() {
        let mut p = Pantalla::nueva(4, 1);
        p.alimentar(b"\x1b[1;31ma\x1b[0mb");
        let a = p.celda(0, 0).expect("celda");
        assert!(a.estilo.negrita);
        assert_eq!(a.estilo.fg, ColorTerm::Indexado(1));
        let b = p.celda(0, 1).expect("celda");
        assert_eq!(b.estilo, Estilo::default(), "`SGR 0` limpia todo");
    }

    /// Los dos SGR que un programa usa para pedir un color exacto, y el índice
    /// alto que NO es lo mismo (`38;5;n` es la paleta, `38;2;…` es RGB).
    #[test]
    fn el_color_exacto_y_el_indice_alto_no_se_confunden() {
        let mut p = Pantalla::nueva(4, 1);
        p.alimentar(b"\x1b[38;2;10;20;30ma\x1b[38;5;200mb\x1b[48;5;7mc");
        assert_eq!(
            p.celda(0, 0).expect("celda").estilo.fg,
            ColorTerm::Rgb(10, 20, 30)
        );
        assert_eq!(
            p.celda(0, 1).expect("celda").estilo.fg,
            ColorTerm::Indexado(200)
        );
        assert_eq!(
            p.celda(0, 2).expect("celda").estilo.bg,
            ColorTerm::Indexado(7)
        );
    }

    /// Un carácter ancho ocupa DOS celdas, y la segunda es estela: si no, el
    /// resto de la línea sale corrido una columna por cada CJK.
    #[test]
    fn un_caracter_ancho_ocupa_dos_celdas() {
        let mut p = Pantalla::nueva(6, 1);
        p.alimentar("日本x".as_bytes());
        assert_eq!(p.celda(0, 0).expect("celda").c, '日');
        assert!(p.celda(0, 1).expect("celda").estela);
        assert_eq!(p.celda(0, 2).expect("celda").c, '本');
        assert!(p.celda(0, 3).expect("celda").estela);
        assert_eq!(p.celda(0, 4).expect("celda").c, 'x');
        assert_eq!(p.fila_texto(0).trim_end(), "日本x");
    }

    /// **Un byte de control no puede acabar en una celda**, que es la garantía
    /// del crate. Se le dan los bytes que el corpus hostil usa para engañar a
    /// un editor de línea, más un UTF-8 roto.
    #[test]
    fn ningun_byte_de_control_llega_a_una_celda() {
        let mut p = Pantalla::nueva(20, 2);
        p.alimentar(b"a\x15b\x01\x7f\x00c\xff\xfe d\x1b[e");
        for fila in 0..2 {
            for col in 0..20 {
                let c = p.celda(fila, col).expect("celda").c;
                assert!(
                    !c.is_control(),
                    "byte de control pintado en {fila},{col}: {c:?}"
                );
            }
        }
        let texto = p.fila_texto(0);
        assert!(texto.contains('a') && texto.contains('b') && texto.contains('c'));
    }

    /// Un OSC entero no pinta nada. Es el caso de norte: el marcador del cwd
    /// del subshell es un OSC, y un panel que lo pintara le enseñaría al lector
    /// la fontanería en cada prompt — el fallo que #142 ya tuvo una vez.
    #[test]
    fn un_osc_no_pinta_nada() {
        let mut p = Pantalla::nueva(20, 1);
        p.alimentar(b"a\x1b]777;norte-cwd;abc123;/tmp\x07b");
        assert_eq!(p.fila_texto(0).trim_end(), "ab");
    }

    #[test]
    fn el_cursor_se_mueve_se_esconde_y_la_pantalla_se_borra() {
        let mut p = Pantalla::nueva(5, 2);
        p.alimentar(b"hola\x1b[?25l\x1b[2J\x1b[H");
        assert!(!p.cursor_visible());
        assert_eq!(p.cursor(), (0, 0));
        assert_eq!(p.fila_texto(0).trim_end(), "");
        p.alimentar(b"\x1b[?25h\x1b[2;3H");
        assert!(p.cursor_visible());
        assert_eq!(
            p.cursor(),
            (1, 2),
            "`CSI H` cuenta desde uno; la rejilla, desde cero"
        );
    }

    #[test]
    fn redimensionar_conserva_lo_que_cabe() {
        let mut p = Pantalla::nueva(6, 2);
        p.alimentar(b"abcdef\r\nghi");
        p.redimensionar(3, 2);
        assert_eq!(p.tamano(), (3, 2));
        assert_eq!(p.fila_texto(0), "abc");
        assert_eq!(p.fila_texto(1), "ghi");
        assert_eq!(
            p.cursor(),
            (1, 3),
            "el cursor no se sale de la rejilla nueva"
        );
    }

    /// **Un movimiento de cursor enorme no tumba nada.**
    ///
    /// `CSI 65535 C` es un parámetro que `vte` entrega tal cual, y sumarlo a la
    /// columna desbordaba el `u16`: en un perfil con comprobaciones —o sea el
    /// `dev` con el que corre toda la suite y el binario que `just link` deja—
    /// eso es un pánico, y lo dispara CUALQUIER fichero que lleve esos ocho
    /// bytes. Un `cat` de algo descargado, un mensaje de commit, un nombre de
    /// fichero impreso por `find`.
    ///
    /// Lo que delataba el fallo estaba a la vista: `A` y `D` eran saturantes y
    /// `B` y `C` no.
    #[test]
    fn un_movimiento_enorme_no_desborda() {
        let mut p = Pantalla::nueva(10, 4);
        p.alimentar(b"a\x1b[65535C\x1b[65535B\x1b[65535A\x1b[65535D");
        let (fila, col) = p.cursor();
        assert!(
            fila < 4 && col < 10,
            "el cursor se quedó fuera: {fila},{col}"
        );
    }

    /// Un carácter ANCHO en una rejilla de UNA columna no cabe ni envolviendo.
    ///
    /// El caso mínimo que encontró la proptest. No es rebuscado: una rejilla
    /// de una columna sale de estrechar la ventana, y una `ä` ancha sale de
    /// cualquier `ls`. El cursor acababa en la columna 2 de una rejilla que
    /// llega hasta la 1, que es la invariante que todo lo demás da por buena.
    #[test]
    fn un_caracter_ancho_en_una_rejilla_de_una_columna_no_saca_el_cursor() {
        let mut p = Pantalla::nueva(1, 2);
        p.alimentar("世".as_bytes());
        let (fila, col) = p.cursor();
        assert!(fila < 2, "fila {fila} fuera de 2");
        assert!(col <= 1, "columna {col} fuera de 1");
        // Y repetirlo tampoco lo saca: el estado sobrevive entre lecturas.
        p.alimentar("界".as_bytes());
        let (_, col) = p.cursor();
        assert!(col <= 1, "columna {col} fuera de 1 tras el segundo");
    }

    /// **Ninguna secuencia tumba la rejilla, la escriba quien la escriba.**
    ///
    /// Es el test que el plan pedía para esta fase y que una lista de bytes
    /// escrita a mano no da: un emulador se alimenta de lo que otro programa
    /// escupa, así que lo que hay que probar no son los quince casos que se
    /// nos ocurran, sino que no hay un decimosexto.
    ///
    /// Se parte además en trozos arbitrarios, porque así es como llega de un
    /// pty y porque el estado que sobrevive entre dos lecturas es justo donde
    /// un parser se rompe.
    #[test]
    fn ninguna_secuencia_tumba_la_rejilla() {
        use proptest::prelude::*;
        proptest!(|(trozos in prop::collection::vec(
            prop::collection::vec(any::<u8>(), 0..64),
            1..8,
        ), ancho in 1u16..40, alto in 1u16..12)| {
            let mut p = Pantalla::nueva(ancho, alto);
            for t in &trozos {
                p.alimentar(t);
            }
            let (fila, col) = p.cursor();
            prop_assert!(fila < alto, "fila {fila} fuera de {alto}");
            prop_assert!(col <= ancho, "columna {col} fuera de {ancho}");
            // Y la invariante del crate aguanta pase lo que pase.
            for f in 0..alto {
                for c in 0..ancho {
                    let celda = p.celda(f, c).expect("dentro de la rejilla");
                    prop_assert!(!celda.c.is_control(), "control en {f},{c}");
                }
            }
        });
    }

    /// El corpus hostil CANÓNICO, dado de comer a la rejilla.
    ///
    /// Los nombres del corpus son lo que un `ls` o un `find` imprimen dentro
    /// del panel, así que son entrada de este crate tanto como de un listado.
    /// Va contra el corpus y no contra una lista local por lo que dice
    /// `norte-frontend`: una lista local deja el fallo fuera del sitio donde
    /// el resto de norte lo busca, y un fixture nuevo no llegaría aquí nunca.
    #[test]
    fn el_corpus_hostil_no_ensucia_ninguna_celda() {
        let mut p = Pantalla::nueva(20, 4);
        for n in norte_testkit::corpus::hostile_names() {
            p.alimentar(&n.bytes);
            p.alimentar(b"\r\n");
            for f in 0..4 {
                for c in 0..20 {
                    let celda = p.celda(f, c).expect("dentro");
                    assert!(
                        !celda.c.is_control(),
                        "«{}» ({}) dejó un control en {f},{c}",
                        n.id,
                        n.why
                    );
                }
            }
        }
    }

    /// Alimentar en trozos tiene que dar lo MISMO que de una vez: un escape
    /// partido entre dos lecturas del pty es lo normal, no lo raro.
    #[test]
    fn un_escape_partido_entre_dos_trozos_se_reconoce() {
        let mut entero = Pantalla::nueva(6, 1);
        entero.alimentar(b"\x1b[31mrojo");
        let mut partido = Pantalla::nueva(6, 1);
        partido.alimentar(b"\x1b[3");
        partido.alimentar(b"1mrojo");
        for col in 0..6 {
            assert_eq!(
                entero.celda(0, col).expect("celda"),
                partido.celda(0, col).expect("celda"),
                "columna {col}"
            );
        }
    }
}
