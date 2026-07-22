//! Parser ANSI-SGR MÍNIMO y SANEADOR de escapes (#29): convierte la salida de
//! un previewer de plugin (p. ej. syntect via `as_24_bit_terminal_escaped`) en
//! líneas con color, interpretando SOLO secuencias SGR (`ESC[…m`) de color de
//! primer plano. Cualquier OTRA secuencia de ESCAPE — movimientos de cursor,
//! borrado de pantalla, OSC (título de ventana), escapes desconocidos — se
//! DESCARTA sin reenviarla jamás a la terminal.
//!
//! Es la frontera de confianza del preview de plugin (regla 9 / superficie
//! hostil): un plugin no puede inyectar secuencias de escape peligrosas en la
//! terminal a través de su `output`; a lo sumo pinta texto coloreado. Los
//! bytes de control SUELTOS (BEL, retroceso…) se conservan en el texto y los
//! enmascara a `�` la capa de display del frontend ([`crate::display_name`],
//! que además trata bidi/invisibles) — el parser solo se ocupa de las
//! secuencias multi-carácter que `display_name` no sabría reconocer. El frontend
//! traduce [`Rgb`] a su propio tipo de color (ratatui/GPUI).

/// Color RGB de 24 bits (primer plano).
pub type Rgb = (u8, u8, u8);

/// Un tramo de texto con un color de primer plano opcional (`None` = color por
/// defecto del tema).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledSpan {
    /// El texto visible del tramo (ya sin secuencias de escape).
    pub text: String,
    /// Color de primer plano, o `None` para el del tema.
    pub fg: Option<Rgb>,
}

/// Una línea = secuencia de tramos con estilo.
pub type StyledLine = Vec<StyledSpan>;

/// Parsea `input` (salida de un previewer) a líneas con estilo, interpretando
/// SOLO SGR de color de primer plano y DESCARTANDO cualquier otra secuencia de
/// escape (saneado — ver el módulo). Las líneas se separan por `\n`; un `\r`
/// final de línea se ignora (CRLF). Siempre devuelve al menos una línea.
///
/// ```
/// use norte_frontend::ansi::{parse_sgr, StyledSpan};
/// // Un tramo rojo de 24 bits + un OSC hostil (fija el título): el color se
/// // conserva, el OSC se descarta entero.
/// let out = parse_sgr("\x1b[38;2;255;0;0mhi\x1b]0;PWNED\x07\x1b[0m fin");
/// assert_eq!(out.len(), 1);
/// assert_eq!(out[0][0], StyledSpan { text: "hi".into(), fg: Some((255, 0, 0)) });
/// assert_eq!(out[0][1], StyledSpan { text: " fin".into(), fg: None });
/// ```
#[must_use]
pub fn parse_sgr(input: &str) -> Vec<StyledLine> {
    let mut lines: Vec<StyledLine> = Vec::new();
    let mut line: StyledLine = Vec::new();
    let mut cur = String::new();
    let mut fg: Option<Rgb> = None;
    let mut chars = input.chars().peekable();

    // Cierra el tramo actual (si tiene texto) con el color vigente.
    let flush = |cur: &mut String, fg: Option<Rgb>, line: &mut StyledLine| {
        if !cur.is_empty() {
            line.push(StyledSpan {
                text: std::mem::take(cur),
                fg,
            });
        }
    };

    while let Some(c) = chars.next() {
        match c {
            '\x1b' => {
                // Secuencia de escape: SOLO CSI-SGR (`ESC[…m`) se interpreta;
                // el resto se consume y descarta.
                if chars.peek() == Some(&'[') {
                    chars.next(); // '['
                    let mut params = String::new();
                    let mut final_byte = None;
                    for pc in chars.by_ref() {
                        // Byte final de un CSI: 0x40..=0x7E.
                        if ('\u{40}'..='\u{7E}').contains(&pc) {
                            final_byte = Some(pc);
                            break;
                        }
                        params.push(pc);
                    }
                    if final_byte == Some('m') {
                        // SGR: aplica al color vigente tras cerrar el tramo.
                        flush(&mut cur, fg, &mut line);
                        apply_sgr(&params, &mut fg);
                    }
                    // Cualquier otro CSI (cursor, borrado…) se descarta.
                } else if chars.peek() == Some(&']') {
                    // OSC (`ESC]…`): título de ventana, hyperlinks… hasta BEL
                    // o ST (`ESC\`). Se consume entero y se descarta.
                    chars.next(); // ']'
                    while let Some(pc) = chars.next() {
                        if pc == '\x07' {
                            break;
                        }
                        if pc == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                } else {
                    // Escape desconocido: descarta el siguiente carácter (si lo
                    // hay) — jamás se reenvía a la terminal.
                    chars.next();
                }
            }
            '\n' => {
                flush(&mut cur, fg, &mut line);
                lines.push(std::mem::take(&mut line));
            }
            '\r' => { /* CR: normalización CRLF — jamás mueve el cursor */ }
            // Otros controles (BEL, retroceso, tab…) se CONSERVAN en el texto:
            // el enmascarado a `�` lo hace la capa de display del frontend
            // ([`crate::display_name`]), que ya trata bidi/invisibles. Aquí solo
            // se sanean las SECUENCIAS DE ESCAPE (arriba), que abarcan varios
            // chars y display_name no sabría reconocer.
            c => cur.push(c),
        }
    }
    flush(&mut cur, fg, &mut line);
    lines.push(line);
    lines
}

/// Aplica una lista de parámetros SGR (`;`-separados) al color de primer plano
/// vigente. Interpreta: `0` (reset), `39` (fg por defecto), `38;2;r;g;b`
/// (24 bits), `38;5;n` (256 → RGB). Consume correctamente `48;…` (fondo) y los
/// ignora; cualquier otro parámetro (negrita, subrayado…) también se ignora.
fn apply_sgr(params: &str, fg: &mut Option<Rgb>) {
    // Un SGR vacío (`ESC[m`) equivale a reset.
    if params.is_empty() {
        *fg = None;
        return;
    }
    let mut it = params.split(';').map(|p| p.parse::<u16>().unwrap_or(0));
    while let Some(code) = it.next() {
        match code {
            0 | 39 => *fg = None,
            38 => *fg = take_extended_color(&mut it).or(*fg),
            48 => {
                // Fondo: consume sus sub-parámetros (para no malinterpretarlos)
                // pero NO lo aplicamos (sin pintar fondo desde un plugin).
                let _ = take_extended_color(&mut it);
            }
            30..=37 => *fg = Some(ansi16_to_rgb(code - 30, false)),
            90..=97 => *fg = Some(ansi16_to_rgb(code - 90, true)),
            _ => {} // negrita/itálica/etc.: ignorado
        }
    }
}

/// Consume un color extendido tras `38`/`48`: `2;r;g;b` (24 bits) o `5;n`
/// (256). `None` si la forma no encaja (sub-parámetros ya consumidos).
fn take_extended_color(it: &mut impl Iterator<Item = u16>) -> Option<Rgb> {
    match it.next()? {
        2 => {
            let r = it.next()?;
            let g = it.next()?;
            let b = it.next()?;
            Some((clamp8(r), clamp8(g), clamp8(b)))
        }
        5 => Some(xterm256_to_rgb(clamp8(it.next()?))),
        _ => None,
    }
}

fn clamp8(v: u16) -> u8 {
    u8::try_from(v).unwrap_or(255)
}

/// Los 16 colores ANSI base → RGB (paleta estándar de xterm).
fn ansi16_to_rgb(idx: u16, bright: bool) -> Rgb {
    const NORMAL: [Rgb; 8] = [
        (0, 0, 0),
        (205, 0, 0),
        (0, 205, 0),
        (205, 205, 0),
        (0, 0, 238),
        (205, 0, 205),
        (0, 205, 205),
        (229, 229, 229),
    ];
    const BRIGHT: [Rgb; 8] = [
        (127, 127, 127),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (92, 92, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    let i = (idx as usize).min(7);
    if bright { BRIGHT[i] } else { NORMAL[i] }
}

/// Índice xterm-256 → RGB: 0..15 base, 16..231 cubo 6×6×6, 232..255 grises.
fn xterm256_to_rgb(n: u8) -> Rgb {
    match n {
        0..=7 => ansi16_to_rgb(u16::from(n), false),
        8..=15 => ansi16_to_rgb(u16::from(n - 8), true),
        16..=231 => {
            let n = n - 16;
            let level = |v: u8| -> u8 { if v == 0 { 0 } else { 55 + v * 40 } };
            (level(n / 36), level((n / 6) % 6), level(n % 6))
        }
        _ => {
            let v = 8 + (n - 232) * 10;
            (v, v, v)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texto_plano_una_linea_sin_color() {
        let out = parse_sgr("hola mundo");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![StyledSpan {
                text: "hola mundo".into(),
                fg: None
            }]
        );
    }

    #[test]
    fn sgr_24_bits_colorea_el_tramo() {
        // ESC[38;2;255;0;0m rojo ESC[0m
        let out = parse_sgr("\x1b[38;2;255;0;0mrojo\x1b[0m fin");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![
                StyledSpan {
                    text: "rojo".into(),
                    fg: Some((255, 0, 0))
                },
                StyledSpan {
                    text: " fin".into(),
                    fg: None
                },
            ]
        );
    }

    #[test]
    fn colores_16_y_256() {
        let out = parse_sgr("\x1b[31mA\x1b[38;5;46mB");
        assert_eq!(
            out[0][0],
            StyledSpan {
                text: "A".into(),
                fg: Some((205, 0, 0))
            }
        );
        // 46 = cubo: n=30 → (0,255,0)
        assert_eq!(out[0][1].text, "B");
        assert_eq!(out[0][1].fg, Some((0, 255, 0)));
    }

    #[test]
    fn saltos_de_linea_y_crlf() {
        let out = parse_sgr("a\r\nb\nc");
        assert_eq!(out.len(), 3);
        assert_eq!(out[0][0].text, "a"); // el \r no aparece
        assert_eq!(out[1][0].text, "b");
        assert_eq!(out[2][0].text, "c");
    }

    /// SANEADO: un OSC hostil (fija el título de la ventana) se descarta
    /// entero — jamás llega a la terminal.
    #[test]
    fn osc_hostil_se_descarta() {
        let out = parse_sgr("antes\x1b]0;PWNED\x07despues");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![StyledSpan {
                text: "antesdespues".into(),
                fg: None
            }]
        );
    }

    /// SANEADO: CSI que NO es SGR (borrar pantalla, mover cursor) se descarta.
    #[test]
    fn csi_no_sgr_se_descarta() {
        let out = parse_sgr("x\x1b[2J\x1b[10;5Hy");
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0],
            vec![StyledSpan {
                text: "xy".into(),
                fg: None
            }]
        );
    }

    /// SANEADO: un escape desnudo (`ESC Z`) se descarta (consume la `Z`); los
    /// bytes de control SUELTOS (BEL, backspace) se CONSERVAN en el texto para
    /// que `display_name` los enmascare a `�` después.
    #[test]
    fn escape_desnudo_se_descarta_controles_sueltos_se_conservan() {
        let out = parse_sgr("a\x07b\x08\x1bZc\td");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0][0].text, "a\x07b\x08c\td");
    }

    #[test]
    fn sgr_vacio_es_reset() {
        let out = parse_sgr("\x1b[31mA\x1b[mB");
        assert_eq!(out[0][0].fg, Some((205, 0, 0)));
        assert_eq!(
            out[0][1],
            StyledSpan {
                text: "B".into(),
                fg: None
            }
        );
    }
}
