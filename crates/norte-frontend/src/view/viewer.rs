//! Estado del viewer (fase 7, spec §6): texto decodificado con detección
//! (vía `norte-encoding`), «recargar como…» y hexview para binarios. Puro y
//! testeable: la lectura la hace `main` vía el core (regla 7).
//!
//! Core SIN i18n (GUI-d T1): el texto de estado localizado vive render-side
//! en cada frontend (`norte-tui::viewer::status`, TUI T2) compuesto sobre los
//! getters de este módulo (`encoding_name`/`eol`/`had_errors`/`is_forced`).

use norte_encoding::{Decoded, Detection, Eol};
use norte_proto::VPath;

/// Cuántas líneas salta una página (fijo, como en los panes).
pub const PAGE: usize = 10;

/// Bytes por fila del hexview.
const HEX_COLS: usize = 16;

/// Ancho de una fila del hexview, en celdas: `offset  hex×16  ascii`.
///
/// Todo ASCII, así que celdas y bytes coinciden. La cuenta es la de
/// [`hex_rows`]: ocho de offset, dos de separación, tres por byte, uno de
/// hueco entre las dos mitades, dos más de separación, y la columna ASCII.
const HEX_ROW_CELLS: usize = 8 + 2 + HEX_COLS * 3 + 1 + 2 + HEX_COLS;

/// Formato de imagen RECONOCIDO por bytes mágicos (no por extensión: el viewer
/// lee CONTENIDO, spec §6). El core NO decodifica (sin dep `image`): solo
/// reconoce y entrega los bytes crudos al frontend, que decide si sabe pintarlos
/// (la GUI GPUI decodifica y muestra la imagen; la TUI cae a hexview vía `rows`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImageFmt {
    /// PNG (`\x89PNG\r\n\x1a\n`).
    Png,
    /// JPEG (`\xFF\xD8\xFF`).
    Jpeg,
    /// GIF (`GIF87a` / `GIF89a`).
    Gif,
    /// BMP (`BM`).
    Bmp,
    /// WebP (contenedor RIFF con marca `WEBP`).
    Webp,
}

impl ImageFmt {
    /// Etiqueta técnica para la barra de estado (literal, no i18n).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ImageFmt::Png => "PNG",
            ImageFmt::Jpeg => "JPEG",
            ImageFmt::Gif => "GIF",
            ImageFmt::Bmp => "BMP",
            ImageFmt::Webp => "WebP",
        }
    }
}

/// Reconoce un formato de imagen por sus bytes MÁGICOS (spec §6: el viewer lee
/// CONTENIDO, jamás confía en la extensión). Puro y barato: solo mira la
/// cabecera, no decodifica ni valida el resto. `None` si no es ninguno de los
/// formatos soportados. La decodificación real (y su validación) la hace el
/// frontend; un falso positivo aquí se cae al fallback del frontend, no rompe.
#[must_use]
pub fn image_format(bytes: &[u8]) -> Option<ImageFmt> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(ImageFmt::Png)
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(ImageFmt::Jpeg)
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Some(ImageFmt::Gif)
    } else if bytes.starts_with(b"BM") {
        Some(ImageFmt::Bmp)
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && &bytes[8..12] == b"WEBP" {
        Some(ImageFmt::Webp)
    } else {
        None
    }
}

/// El presupuesto de píxeles de una preview: 40 megapíxeles.
///
/// No es un límite de estética sino de MEMORIA. Un PNG de 64 KB puede declarar
/// 60000×60000 y costarle gigabytes al decodificador: es la bomba de
/// descompresión, y la única defensa barata es leerle la cabecera y negarse
/// ANTES de darle los bytes a nadie que decodifique. Cuarenta megapíxeles
/// cubren con holgura cualquier foto real (una de 50 Mpx es un sensor de gama
/// alta) y son ~160 MB en RGBA, que es caro pero no letal.
pub const PIXEL_BUDGET: u64 = 40_000_000;

/// Las dimensiones que la cabecera DECLARA, sin decodificar nada.
///
/// `None` cuando no se reconoce el formato, cuando la cabecera está incompleta
/// o cuando dice algo que no se entiende: quien llama trata ese `None` como
/// «no se puede prometer nada de esta imagen» y se niega, que es lo contrario
/// de tratarlo como «adelante».
///
/// Lee OFFSETS FIJOS y no asigna nada en función de lo que lea. Es código que
/// mira bytes de un tercero para decidir un número, así que es del tipo
/// aburrido a propósito: sin bucles sobre longitudes del fichero salvo el
/// recorrido acotado de segmentos de JPEG, y sin aritmética que pueda
/// desbordar (todo en `u64`).
///
/// ```
/// use norte_frontend::viewer::image_dimensions;
/// // Un PNG mínimo: firma, longitud del IHDR, tipo, y ancho/alto.
/// let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
/// png.extend_from_slice(&[0, 0, 0, 13]);
/// png.extend_from_slice(b"IHDR");
/// png.extend_from_slice(&800u32.to_be_bytes());
/// png.extend_from_slice(&600u32.to_be_bytes());
/// assert_eq!(image_dimensions(&png), Some((800, 600)));
/// assert_eq!(image_dimensions(b"no soy una imagen"), None);
/// ```
#[must_use]
pub fn image_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    match image_format(bytes)? {
        // IHDR es SIEMPRE el primer chunk y está en un offset fijo.
        ImageFmt::Png => (bytes.len() >= 24 && &bytes[12..16] == b"IHDR")
            .then(|| (be32(bytes, 16), be32(bytes, 20))),
        // Logical Screen Descriptor, little-endian, justo tras la firma.
        ImageFmt::Gif => {
            (bytes.len() >= 10).then(|| (u32::from(le16(bytes, 6)), u32::from(le16(bytes, 8))))
        }
        // DIB header: ancho y alto con signo; el alto NEGATIVO significa
        // filas de arriba abajo, no una imagen de tamaño negativo.
        ImageFmt::Bmp => (bytes.len() >= 26).then(|| {
            (
                le32(bytes, 18).unsigned_abs(),
                le32(bytes, 22).unsigned_abs(),
            )
        }),
        ImageFmt::Webp => webp_dimensions(bytes),
        ImageFmt::Jpeg => jpeg_dimensions(bytes),
    }
}

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> i32 {
    i32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// WebP tiene TRES sabores y cada uno guarda el tamaño en otro sitio. Uno que
/// no se reconozca es `None`: negarse es la respuesta correcta a «no sé».
fn webp_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    if b.len() < 30 {
        return None;
    }
    match &b[12..16] {
        // Simple lossy: el keyframe de VP8 lleva 14 bits por eje.
        b"VP8 " => Some((
            u32::from(le16(b, 26) & 0x3FFF),
            u32::from(le16(b, 28) & 0x3FFF),
        )),
        // Lossless: 14 bits por eje, empaquetados y menos uno.
        b"VP8L" => {
            let v = u32::from_le_bytes([b[21], b[22], b[23], b[24]]);
            Some(((v & 0x3FFF) + 1, ((v >> 14) & 0x3FFF) + 1))
        }
        // Extendido: 24 bits por eje, menos uno.
        // El `+ 1` es del VALOR entero, no del último desplazamiento: sin
        // los paréntesis se ata al `<< 16` y el ancho sale mal por 65536.
        // Lo cazó clippy, y es justo la clase de error que esta función
        // tiene prohibida.
        b"VP8X" => Some((
            (u32::from(b[24]) | (u32::from(b[25]) << 8) | (u32::from(b[26]) << 16)) + 1,
            (u32::from(b[27]) | (u32::from(b[28]) << 8) | (u32::from(b[29]) << 16)) + 1,
        )),
        _ => None,
    }
}

/// JPEG no tiene offset fijo: hay que recorrer segmentos hasta el SOF. El
/// recorrido está ACOTADO por la longitud de lo leído y avanza siempre, así
/// que no puede quedarse dando vueltas sobre un fichero manipulado.
fn jpeg_dimensions(b: &[u8]) -> Option<(u32, u32)> {
    let mut i = 2usize;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            // Fuera de sincronía: no se adivina, se abandona.
            return None;
        }
        let marcador = b[i + 1];
        // SOF0..SOF15, saltando los que no llevan tamaño (DHT, JPG, DAC).
        if (0xC0..=0xCF).contains(&marcador) && !matches!(marcador, 0xC4 | 0xC8 | 0xCC) {
            return Some((
                u32::from(u16::from_be_bytes([b[i + 7], b[i + 8]])),
                u32::from(u16::from_be_bytes([b[i + 5], b[i + 6]])),
            ));
        }
        let largo = usize::from(u16::from_be_bytes([b[i + 2], b[i + 3]]));
        if largo < 2 {
            return None;
        }
        i += 2 + largo;
    }
    None
}

/// Preview producido por un plugin (M4-P5): reemplaza la vista cruda mientras
/// está presente. Líneas con estilo (#29): la salida del plugin se parsea con
/// el saneador ANSI-SGR ([`crate::ansi::parse_sgr`]) — que DESCARTA cualquier
/// escape peligroso y deja solo color de primer plano — y cada tramo se
/// enmascara ([`crate::display_name`]): texto de un TERCERO, jamás confiado.
pub struct PluginPreviewView {
    /// Nombre legible del plugin previewer (ya enmascarado), para el indicador.
    pub plugin_name: String,
    /// La decodificación host-side del fichero fue LOSSY (#101,
    /// `PluginPreview::lossy` del wire): los `�` de la salida vienen de una
    /// decodificación fallida, no del fichero. El frontend lo lee por
    /// [`Viewer::preview_lossy`] y lo señala junto al indicador «via …».
    /// Privado: solo lo fijan los dos constructores del `Viewer`.
    lossy: bool,
    /// Líneas con color, saneadas y enmascaradas.
    styled: Vec<crate::ansi::StyledLine>,
}

/// El viewer abierto sobre un archivo.
pub struct Viewer {
    /// El archivo mostrado.
    pub path: VPath,
    /// Lo leído (posiblemente truncado al presupuesto del viewer).
    bytes: Vec<u8>,
    /// `true` si el archivo seguía (solo se leyó la cabecera).
    pub truncated: bool,
    /// Hexview activo (automático en binarios NO-imagen; toggle manual).
    pub hex: bool,
    /// Formato de imagen reconocido por bytes mágicos (`recompute`), o `None`.
    /// Cuando es `Some` y no hay encoding forzado, el viewer está en modo
    /// imagen: la GUI la pinta ([`Viewer::is_image`]); frontends sin render de
    /// imagen (TUI) caen a hexview (los bytes siguen disponibles vía `rows`).
    image: Option<ImageFmt>,
    /// Encoding forzado por «recargar como…» (None = detección).
    forced: Option<&'static norte_encoding::Encoding>,
    /// Primera línea visible.
    pub scroll: usize,
    /// Primera COLUMNA visible, en celdas de terminal.
    ///
    /// Sin esto, una línea más ancha que la ventana —un HTML minificado, un
    /// CSV, un log— se pintaba recortada y el resto era inalcanzable: el visor
    /// no envuelve, así que lo que no cabe no está en ninguna parte.
    ///
    /// Cada modo tiene su propio ancho, así que al cambiar de modo vuelve a
    /// cero: una columna de la vista anterior no nombra la misma columna aquí.
    hscroll: usize,
    /// La línea de TEXTO más ancha, en celdas.
    ///
    /// El tope de verdad lo da [`Self::max_cols`], que además sabe del
    /// hexadecimal — este campo es solo la mitad de texto de esa respuesta.
    ///
    /// Se mide una vez al decodificar y no por ventana. Medir solo lo visible
    /// haría que el tope cambiara al bajar, y el texto saltaría de lado sin
    /// que nadie hubiera pulsado nada.
    max_cols_texto: usize,
    /// El zoom de la IMAGEN, en porcentaje de lo que ocuparía ajustada.
    /// `None` = ajustar, y es como se abre siempre: lo primero que se quiere
    /// de una imagen es verla entera.
    ///
    /// [`Viewer::ZOOM_ACTUAL`] es el centinela de «tamaño real». Vive aquí y
    /// no en el frontend porque los dos lo necesitan igual y porque es
    /// estado del visor, como el hexadecimal o el encoding forzado.
    zoom: Option<u16>,
    /// Preview de un plugin (M4-P5): si está, REEMPLAZA la vista cruda y la
    /// decodificación (bytes/encoding se ignoran; `lines` ya enmascaradas).
    plugin_preview: Option<PluginPreviewView>,
    // ---- cache de decodificación (se recomputa al cambiar encoding) ----
    text: String,
    encoding_name: &'static str,
    eol: Eol,
    had_errors: bool,
    lines: usize,
}

impl Viewer {
    /// Struct base con todos los campos en su cero (sin decodificar aún).
    fn base(path: VPath, bytes: Vec<u8>, truncated: bool) -> Self {
        Self {
            path,
            bytes,
            truncated,
            hex: false,
            image: None,
            forced: None,
            scroll: 0,
            hscroll: 0,
            max_cols_texto: 0,
            // Una imagen se abre AJUSTADA: lo primero que se quiere de ella
            // es verla entera.
            zoom: None,
            plugin_preview: None,
            text: String::new(),
            encoding_name: "",
            eol: Eol::None,
            had_errors: false,
            lines: 0,
        }
    }

    /// Viewer sobre `bytes` (ya leídos): detecta encoding y binario.
    #[must_use]
    pub fn new(path: VPath, bytes: Vec<u8>, truncated: bool) -> Self {
        let mut v = Self::base(path, bytes, truncated);
        v.recompute();
        v
    }

    /// Viewer en modo preview de plugin (M4-P5): pinta la salida del plugin en
    /// vez de la vista cruda. El `output` es texto de un TERCERO: se pasa por el
    /// saneador ANSI-SGR (#29 — [`crate::ansi::parse_sgr`]: descarta escapes
    /// peligrosos, deja solo color de primer plano) y CADA tramo se enmascara
    /// con [`crate::display_name`] (controles/bidi/invisibles → `�`); el
    /// `plugin_name` igual.
    #[must_use]
    pub fn with_plugin_preview(
        path: VPath,
        plugin_name: String,
        output: &str,
        lossy: bool,
    ) -> Self {
        let styled: Vec<crate::ansi::StyledLine> = crate::ansi::parse_sgr(output)
            .into_iter()
            .map(|line| {
                line.into_iter()
                    .map(|span| crate::ansi::StyledSpan {
                        text: crate::display_name(span.text.as_bytes()).0,
                        role: None, // ANSI-SGR no tiene concepto de rol (G3a, ver ansi.rs)
                        fg: span.fg,
                        bg: None,
                    })
                    .collect()
            })
            .collect();
        let plugin_name = crate::display_name(&plugin_name.into_bytes()).0;
        let mut v = Self::base(path, Vec::new(), false);
        v.max_cols_texto = ancho_de_estilo(&styled);
        v.plugin_preview = Some(PluginPreviewView {
            plugin_name,
            lossy,
            styled,
        });
        v
    }

    /// Viewer en modo preview de plugin CON ESTILO (G3a, ADR 0037): gemelo
    /// de [`Self::with_plugin_preview`] que consume `lines` YA
    /// ESTRUCTURADAS (`SpanWire`, del wire `plugin.preview_styled`) en vez
    /// de una salida ANSI-SGR que sanear. Cada `text` de span es texto de un
    /// TERCERO — se enmascara IGUAL que la ruta ANSI (`crate::display_name`,
    /// mismo saneado, no una copia paralela); cada `role` es un nombre de
    /// `norte_theme::Role` que llega SIN VALIDAR por el wire (`norte-core`
    /// no depende de `norte-theme` — ver el rustdoc de
    /// `Backend::plugin_preview_styled`) y se valida AQUÍ, la frontera
    /// donde el frontend por fin conoce el tema
    /// (`norte_theme::Role::from_kebab_requestable`): un nombre desconocido
    /// —o uno del CROMO o del ESTADO de la ventana, que desde la spec
    /// 2026-09-11 (F2) no son pedibles por un plugin— colapsa a
    /// `None` — jamás un panic ni una cadena libre que otra capa deba
    /// re-interpretar (ADR 0037, mismo criterio que un tema con datos
    /// parciales, ADR 0020). `fg` es el fallback RGB crudo, ya acotado por
    /// el wire (`[u8; 3]` siempre representable) — se copia tal cual; `bg`
    /// (0.66.0) igual, y no hay rol que le gane: un fondo es un fondo.
    #[must_use]
    pub fn with_plugin_preview_styled(
        path: VPath,
        plugin_name: String,
        lines: &[Vec<norte_proto::methods::SpanWire>],
        lossy: bool,
    ) -> Self {
        let styled: Vec<crate::ansi::StyledLine> = lines
            .iter()
            .map(|line| line.iter().map(crate::ansi::span_de_wire).collect())
            .collect();
        let plugin_name = crate::display_name(&plugin_name.into_bytes()).0;
        let mut v = Self::base(path, Vec::new(), false);
        v.max_cols_texto = ancho_de_estilo(&styled);
        v.plugin_preview = Some(PluginPreviewView {
            plugin_name,
            lossy,
            styled,
        });
        v
    }

    /// Las filas visibles del preview de plugin CON estilo (#29), desde
    /// `scroll` y desde [`Self::hscroll`]; `None` si el viewer no está en modo
    /// preview de plugin. El frontend traduce [`crate::ansi::Rgb`] a su tipo de
    /// color y las pinta.
    ///
    /// Devuelve líneas PROPIAS y no referencias porque el desplazamiento
    /// horizontal parte tramos: la salida de un plugin se desplaza igual que
    /// el texto crudo — un previewer de CSV o de JSON produce líneas largas por
    /// el mismo motivo que el fichero.
    #[must_use]
    pub fn plugin_styled_rows(&self, height: usize) -> Option<Vec<crate::ansi::StyledLine>> {
        self.plugin_preview.as_ref().map(|p| {
            p.styled
                .iter()
                .skip(self.scroll)
                .take(height)
                .map(|l| desplazar_estilo(l, self.hscroll))
                .collect()
        })
    }

    /// El nombre del plugin si el viewer está en modo preview (para el
    /// indicador «via …» de la cabecera), o `None` si es la vista cruda.
    #[must_use]
    pub fn preview_plugin(&self) -> Option<&str> {
        self.plugin_preview.as_ref().map(|p| p.plugin_name.as_str())
    }

    /// `true` si el viewer está en modo preview de plugin Y la decodificación
    /// host-side del fichero fue LOSSY (#101): el frontend pinta un aviso junto
    /// al indicador «via …». `false` para la vista cruda (que marca su propio
    /// [`Self::had_errors`]) o para un preview no-lossy.
    #[must_use]
    pub fn preview_lossy(&self) -> bool {
        self.plugin_preview.as_ref().is_some_and(|p| p.lossy)
    }

    /// Re-decodifica los bytes y pone al día lo derivado.
    ///
    /// Con un preview de plugin delante NO HACE NADA, y eso es un arreglo: en
    /// ese modo `bytes` está vacío, así que la detección decía «texto vacío» y
    /// se llevaba por delante el ancho medido de la salida del plugin —o sea
    /// que pulsar «recargar como…» sobre un preview de un CSV ancho apagaba su
    /// barra horizontal y topaba el desplazamiento en cero, en silencio.
    /// `total_rows` y `rows` ya preguntaban primero por el preview; esto es lo
    /// que faltaba de la misma regla.
    fn recompute(&mut self) {
        if self.plugin_preview.is_some() {
            return;
        }
        let encoding = self.forced.or(match norte_encoding::detect(&self.bytes) {
            Detection::Text { encoding, .. } => Some(encoding),
            Detection::Binary => None,
        });
        if let Some(enc) = encoding {
            // Forzado = sin BOM-sniffing (el usuario MANDA, spec §6.2);
            // truncado = la cola partida queda pendiente, no es pérdida.
            let Decoded {
                text,
                encoding,
                had_errors,
            } = if self.forced.is_some() {
                norte_encoding::decode_forced(&self.bytes, enc, !self.truncated)
            } else {
                norte_encoding::decode(&self.bytes, enc, !self.truncated)
            };
            self.eol = norte_encoding::detect_eol(&text);
            // Para PINTAR: todo EOL (incl. CR de Mac clásico) parte línea.
            let text = text.replace("\r\n", "\n").replace('\r', "\n");
            self.lines = text.lines().count();
            // La más ancha YA renderizada: los tabs se expanden antes de
            // pintar, así que medir el texto crudo daría un tope corto y
            // dejaría la cola de una línea con tabulaciones inalcanzable.
            self.max_cols_texto = text
                .lines()
                .map(|l| crate::display::cells(&render_line(l)))
                .max()
                .unwrap_or(0);
            self.text = text;
            self.encoding_name = encoding.name();
            self.had_errors = had_errors;
            self.hex = false;
            // Texto (o forzado a texto): no es una imagen a mostrar como tal.
            self.image = None;
        } else {
            // Binario: jamás decodificar a ciegas (spec §6) — hexview. Además
            // reconocemos si es una imagen (bytes mágicos) para que la GUI la
            // PINTE ([`Viewer::is_image`]); el hexview sigue activo como fallback
            // de los frontends sin render de imagen (TUI) — `rows` no cambia.
            self.image = image_format(&self.bytes);
            self.hex = true;
            self.encoding_name = "";
            self.eol = Eol::None;
            self.had_errors = false;
            self.text = String::new();
            self.lines = 0;
            // Sin texto no hay línea de texto que medir; el tope de la vista
            // hexadecimal lo pone `max_cols`, que sabe de su ancho fijo.
            self.max_cols_texto = 0;
        }
        self.scroll = 0;
        self.hscroll = 0;
    }

    /// «Recargar como…»: siguiente encoding del ciclo (spec §6). En un
    /// binario fuerza la PRIMERA decodificación de texto del ciclo.
    pub fn cycle_encoding(&mut self) {
        let cycle = norte_encoding::reload_cycle();
        let next = match self.forced {
            None => 0,
            Some(cur) => cycle
                .iter()
                .position(|e| std::ptr::eq(*e, cur))
                .map_or(0, |i| (i + 1) % cycle.len()),
        };
        self.forced = Some(cycle[next]);
        self.recompute();
    }

    /// Vuelve a la detección automática (y al hexview si era binario).
    pub fn reset_encoding(&mut self) {
        self.forced = None;
        self.recompute();
    }

    /// Alterna el hexview manualmente (el texto decodificado se conserva).
    /// El scroll se reclampa: los totales de fila difieren entre modos.
    pub fn toggle_hex(&mut self) {
        self.hex = !self.hex;
        self.scroll = self.scroll.min(self.total_rows().saturating_sub(1));
        // Y el horizontal a cero: las dos vistas tienen anchos distintos, así
        // que la columna 40 del texto no es la columna 40 del volcado, y
        // conservarla enseñaría una que el lector no eligió.
        self.hscroll = 0;
    }

    /// Total de filas visibles en el modo actual.
    #[must_use]
    pub fn total_rows(&self) -> usize {
        if let Some(p) = &self.plugin_preview {
            p.styled.len()
        } else if self.hex {
            self.bytes.len().div_ceil(HEX_COLS)
        } else {
            self.lines
        }
    }

    /// Baja `n` filas (con tope).
    pub fn scroll_down(&mut self, n: usize) {
        self.scroll = (self.scroll + n).min(self.total_rows().saturating_sub(1));
    }

    /// Sube `n` filas.
    pub fn scroll_up(&mut self, n: usize) {
        self.scroll = self.scroll.saturating_sub(n);
    }

    /// Al principio.
    pub fn scroll_top(&mut self) {
        self.scroll = 0;
    }

    /// Al final.
    pub fn scroll_bottom(&mut self) {
        self.scroll = self.total_rows().saturating_sub(1);
    }

    /// La primera columna visible, en celdas.
    #[must_use]
    pub const fn hscroll(&self) -> usize {
        self.hscroll
    }

    /// La línea más ancha, en celdas: cuánto hay a lo ancho.
    ///
    /// El frontend lo compara con el ancho de su ventana para decidir si dibuja
    /// barra horizontal.
    ///
    /// **Depende del MODO, igual que [`Self::total_rows`].** El hexadecimal
    /// tiene su propio ancho —77 celdas: offset, dieciséis bytes y su
    /// columna ASCII— que en un hueco partido o en un terminal estrecho no
    /// cabe, y negarle el eje horizontal dejaba esa columna ASCII
    /// inalcanzable. Leer el ancho del TEXTO en modo hexadecimal era además
    /// una barra que se movía sobre un contenido que no se movía.
    #[must_use]
    pub const fn max_cols(&self) -> usize {
        if self.hex {
            HEX_ROW_CELLS
        } else {
            self.max_cols_texto
        }
    }

    /// Izquierda `n` columnas.
    pub const fn scroll_left(&mut self, n: usize) {
        self.hscroll = self.hscroll.saturating_sub(n);
    }

    /// Derecha `n` columnas, sin pasarse del final de la línea más larga.
    ///
    /// El tope deja SIEMPRE una columna a la vista: un visor desplazado más
    /// allá de todo su contenido es una pantalla en blanco de la que solo se
    /// sale a ciegas.
    pub fn scroll_right(&mut self, n: usize) {
        self.hscroll = (self.hscroll + n).min(self.max_cols().saturating_sub(1));
    }

    /// Las filas visibles desde `scroll`, ya formateadas para el terminal:
    /// tabs EXPANDIDOS (ratatui los borraría: columnas colapsadas en
    /// silencio) y el resto de controles enmascarados a `�` (un `.ans` con
    /// ESC se ve alterado, jamás sin marca) — misma política que los
    /// nombres (spec §6).
    ///
    /// Y recortadas por la IZQUIERDA a [`Self::hscroll`], en los TRES modos —
    /// texto, hexadecimal y preview de plugin. El recorte se hace aquí, sobre
    /// la línea ya renderizada, y no en cada frontend: dos recortes son dos
    /// formas de contar columnas que un día no coinciden.
    #[must_use]
    pub fn rows(&self, height: usize) -> Vec<String> {
        if let Some(p) = &self.plugin_preview {
            // Texto plano PROYECTADO del recorte con estilo, no un segundo
            // recorte: la misma línea no puede pintarse distinta con color y
            // sin él.
            p.styled
                .iter()
                .skip(self.scroll)
                .take(height)
                .map(|line| {
                    desplazar_estilo(line, self.hscroll)
                        .iter()
                        .map(|s| s.text.as_str())
                        .collect()
                })
                .collect()
        } else if self.hex {
            hex_rows(&self.bytes, self.scroll, height)
                .into_iter()
                .map(|f| self.recortada(f))
                .collect()
        } else {
            self.text
                .lines()
                .skip(self.scroll)
                .take(height)
                .map(|l| self.recortada(render_line(l)))
                .collect()
        }
    }

    /// Una fila ya renderizada, desplazada a [`Self::hscroll`].
    fn recortada(&self, fila: String) -> String {
        if self.hscroll == 0 {
            return fila;
        }
        crate::display::skip_cells(&fila, self.hscroll)
    }

    /// Nombre del encoding decodificado (`"UTF-8"`…), o `""` si es binario.
    #[must_use]
    pub fn encoding_name(&self) -> &str {
        self.encoding_name
    }

    /// El fin de línea detectado (solo significativo en modo texto).
    #[must_use]
    pub fn eol(&self) -> norte_encoding::Eol {
        self.eol
    }

    /// Hubo pérdidas al decodificar (bytes inválidos → `�`).
    #[must_use]
    pub fn had_errors(&self) -> bool {
        self.had_errors
    }

    /// El encoding está FORZADO por «recargar como…» (no es la detección).
    #[must_use]
    pub fn is_forced(&self) -> bool {
        self.forced.is_some()
    }

    /// `true` si el contenido es una imagen reconocida (bytes mágicos) y no se
    /// ha forzado una decodificación de texto («recargar como…»). El frontend
    /// con render de imagen (GUI) PINTA la imagen a través de éste. El core NO
    /// decodifica: solo reconoce. Nota: NO depende de `hex` — un frontend
    /// gráfico muestra siempre la imagen; el hexview crudo sigue disponible
    /// por `rows` para quien no sepa pintarla.
    ///
    /// La TUI NO usa este getter para decidir si pide píxeles por el
    /// protocolo de kitty (fase 5 WOW): es `false` en cuanto un previewer de
    /// plugin sustituye la vista cruda por su propio `PluginPreview`, y esa
    /// decisión necesita saber si el FICHERO es una imagen con independencia
    /// de qué previewer ganó — mira los bytes por su cuenta con
    /// [`image_format`] antes de que la cadena de preview tenga oportunidad
    /// de esconder el formato (`norte-tui::viewer_open`). Cuando no pinta
    /// píxeles, cae a `rows` (hexview) igual que siempre.
    #[must_use]
    pub fn is_image(&self) -> bool {
        self.plugin_preview.is_none() && self.image.is_some()
    }

    /// El formato de imagen reconocido (para la barra de estado), o `None` si
    /// no está en modo imagen. Solo `Some` cuando [`Viewer::is_image`].
    #[must_use]
    pub fn image_kind(&self) -> Option<ImageFmt> {
        self.is_image().then_some(self.image).flatten()
    }

    /// Los bytes crudos de la imagen para que el frontend los decodifique
    /// (regla 7: el core no decodifica), o `None` si no está en modo imagen.
    /// Pueden estar TRUNCADOS (`self.truncated`): el decodificador del frontend
    /// debe tolerar un decode fallido y caer a un estado de error, no romper.
    #[must_use]
    pub fn image_bytes(&self) -> Option<&[u8]> {
        self.is_image().then_some(self.bytes.as_slice())
    }

    /// El zoom de la imagen: `None` = AJUSTAR, que es como se abre.
    ///
    /// Un porcentaje y no un factor porque es lo que se enseña en la barra de
    /// estado, y porque el ciclo de peldaños se escribe en porcentajes.
    #[must_use]
    pub fn zoom(&self) -> Option<u16> {
        self.zoom
    }

    /// El zoom en porcentaje contra el tamaño AJUSTADO, ya resuelto: lo que
    /// multiplica el sitio que la imagen ocuparía sola.
    ///
    /// Ajustar es el 100 %, así que quien pinta multiplica siempre y no tiene
    /// que saber que `None` significa algo.
    #[must_use]
    pub fn zoom_pct(&self) -> u16 {
        self.zoom.unwrap_or(100)
    }

    /// Acercar un peldaño.
    pub fn zoom_in(&mut self) {
        self.zoom = Some(Self::escalon_arriba(self.zoom_pct()));
    }

    /// Alejar un peldaño.
    pub fn zoom_out(&mut self) {
        self.zoom = Some(Self::escalon_abajo(self.zoom_pct()));
    }

    /// Volver a AJUSTAR: la imagen entera dentro de lo que hay.
    pub fn zoom_fit(&mut self) {
        self.zoom = None;
        self.scroll = 0;
        self.hscroll = 0;
    }

    /// Cota de acercamiento, en porcentaje del ajustado.
    pub const ZOOM_MAX: u16 = 800;
    /// Cota de alejamiento. Por debajo, la imagen deja de decir nada.
    pub const ZOOM_MIN: u16 = 25;

    /// Los peldaños. Se sube y se baja por ellos y no multiplicando, para que
    /// acercar y alejar la misma cantidad de veces devuelva al MISMO sitio:
    /// con un factor, `100 × 1.25 ÷ 1.25` es 99 o 101 según redondeo, y el
    /// lector acaba en un zoom que no pidió y del que no puede salir.
    const ESCALONES: [u16; 9] = [25, 50, 75, 100, 150, 200, 300, 400, 800];

    fn escalon_arriba(pct: u16) -> u16 {
        Self::ESCALONES
            .into_iter()
            .find(|&e| e > pct)
            .unwrap_or(Self::ZOOM_MAX)
    }

    fn escalon_abajo(pct: u16) -> u16 {
        Self::ESCALONES
            .into_iter()
            .rev()
            .find(|&e| e < pct)
            .unwrap_or(Self::ZOOM_MIN)
    }
}

/// Ancho de tab del viewer (fijo en M1).
const TAB_WIDTH: usize = 8;

/// Prepara una línea para el terminal: tabs a espacios (tab stops de
/// [`TAB_WIDTH`]) y el resto de HAZARDS enmascarados a `�` — MISMA política que
/// los nombres (`is_terminal_hazard`: controles C0/C1, overrides/aislantes bidi,
/// invisibles, separadores Zl/Zp, TAG chars). `is_control()` a secas dejaba
/// pasar bidi (U+202E) e invisibles (ZWSP) crudos: un `.txt` con RLO falsificaba
/// el orden visual (Trojan Source, CVE-2021-42574) en la GUI GPUI —que reordena
/// bidi en el shaping— y en terminales que honran bidi (encoding-auditor GUI-d).
///
/// El tab stop se cuenta por CELDAS, no por caracteres: con `col += 1` por
/// carácter, un ideograma antes de un tab movía el stop una columna y el resto
/// de la línea quedaba desalineado respecto a sus vecinas.
fn render_line(line: &str) -> String {
    use unicode_width::UnicodeWidthChar;
    let mut out = String::with_capacity(line.len());
    let mut col = 0usize;
    for c in line.chars() {
        if c == '\t' {
            let next = (col / TAB_WIDTH + 1) * TAB_WIDTH;
            for _ in col..next {
                out.push(' ');
            }
            col = next;
        } else if norte_encoding::is_terminal_hazard(c) {
            out.push('\u{FFFD}');
            col += 1;
        } else {
            out.push(c);
            // `unwrap_or(1)`: `None` es un CONTROL, y los controles ya se
            // fueron por la rama de arriba. Uno se pinta ocupando algo.
            col += UnicodeWidthChar::width(c).unwrap_or(1);
        }
    }
    out
}

/// La línea con estilo más ancha, en celdas.
fn ancho_de_estilo(styled: &[crate::ansi::StyledLine]) -> usize {
    styled
        .iter()
        .map(|l| l.iter().map(|s| crate::display::cells(&s.text)).sum())
        .max()
        .unwrap_or(0)
}

/// Una línea con estilo desplazada `n` celdas a la izquierda.
///
/// **Es la ÚNICA implementación del corte por la izquierda de una fila.** La
/// versión de texto plano ([`Viewer::rows`] en modo preview) es su proyección,
/// no una segunda cuenta: cuando eran dos, el corte que caía justo en el borde
/// de un tramo limpiaba las marcas huérfanas por una ruta y no por la otra, o
/// sea que la misma línea se pintaba distinta con color y sin él.
///
/// Los tramos que quedan enteros a la izquierda del corte se van; el que lo
/// cruza se recorta CONSERVANDO su color, que es lo que distingue esta función
/// de recortar la cadena entera y perder los tramos. Los tramos que se quedan
/// vacíos no se emiten: un `<span>` sin texto no pinta nada y sí ensucia el DOM.
fn desplazar_estilo(linea: &crate::ansi::StyledLine, n: usize) -> crate::ansi::StyledLine {
    if n == 0 {
        return linea.clone();
    }
    let mut resto = n;
    let mut cortado = false;
    let mut out: crate::ansi::StyledLine = Vec::new();
    for span in linea {
        let texto = if cortado {
            span.text.clone()
        } else {
            let ancho = crate::display::cells(&span.text);
            if resto > 0 && ancho <= resto {
                // Este tramo entero se queda a la izquierda del corte.
                resto -= ancho;
                continue;
            }
            cortado = true;
            crate::display::skip_cells(&span.text, resto)
        };
        if texto.is_empty() {
            continue;
        }
        out.push(crate::ansi::StyledSpan {
            text: texto,
            ..span.clone()
        });
    }
    // El corte pudo caer EXACTO en el borde de un tramo, y entonces
    // `skip_cells` no tuvo nada que saltar y no limpió nada. La marca huérfana
    // se tira igual: quién la pinta no puede depender de dónde el plugin puso
    // sus fronteras de color.
    if let Some(primero) = out.first_mut() {
        let limpio = crate::display::sin_marcas_de_cabeza(&primero.text);
        if limpio.len() != primero.text.len() {
            primero.text = limpio.to_owned();
        }
    }
    out.retain(|s| !s.text.is_empty());
    out
}

/// Filas del hexview: `offset  hex×16  ascii`.
fn hex_rows(bytes: &[u8], scroll: usize, height: usize) -> Vec<String> {
    use std::fmt::Write;
    let mut out = Vec::new();
    for row in scroll..(scroll + height) {
        let start = row * HEX_COLS;
        if start >= bytes.len() {
            break;
        }
        let chunk = &bytes[start..(start + HEX_COLS).min(bytes.len())];
        let mut line = format!("{start:08x}  ");
        for (i, b) in chunk.iter().enumerate() {
            let _ = write!(line, "{b:02x} ");
            if i == 7 {
                line.push(' ');
            }
        }
        let hexw = 8 + 2 + HEX_COLS * 3 + 1;
        while line.len() < hexw + 2 {
            line.push(' ');
        }
        for b in chunk {
            line.push(if (0x20..0x7F).contains(b) {
                *b as char
            } else {
                '.'
            });
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{PIXEL_BUDGET, Viewer, image_dimensions};
    use norte_proto::VPath;

    fn vp() -> VPath {
        VPath::parse("file:///f").unwrap()
    }

    /// **El visor se desplaza a lo ANCHO.**
    ///
    /// El visor no envuelve, así que sin esto la cola de una línea larga —un
    /// HTML minificado, un CSV, un log— no estaba en ninguna parte: se pintaba
    /// recortada y no había forma de llegar a ella.
    #[test]
    fn el_visor_se_desplaza_a_lo_ancho_con_tope() {
        let texto = b"0123456789\ncorta\n".to_vec();
        let mut v = Viewer::new(vp(), texto, false);
        assert_eq!(v.max_cols(), 10, "la línea más ancha manda");
        assert_eq!(v.hscroll(), 0);
        assert_eq!(v.rows(2), vec!["0123456789", "corta"]);

        v.scroll_right(4);
        assert_eq!(v.hscroll(), 4);
        assert_eq!(
            v.rows(2),
            vec!["456789", "a"],
            "cada fila se recorta por la misma columna"
        );

        // El tope deja SIEMPRE una columna: desplazarse más allá de todo el
        // contenido es una pantalla en blanco de la que se sale a ciegas.
        v.scroll_right(1000);
        assert_eq!(v.hscroll(), 9);
        assert_eq!(v.rows(1), vec!["9"]);

        v.scroll_left(1000);
        assert_eq!(v.hscroll(), 0);
        assert_eq!(v.rows(1), vec!["0123456789"]);
    }

    /// El tope se mide sobre la línea YA renderizada: los tabs se expanden
    /// antes de pintar, así que medir el texto crudo dejaría su cola
    /// inalcanzable.
    #[test]
    fn el_tope_horizontal_cuenta_los_tabs_expandidos() {
        let v = Viewer::new(vp(), b"\tab\n".to_vec(), false);
        assert_eq!(v.max_cols(), 10, "un tab son 8 columnas, y luego `ab`");
    }

    /// **El hexadecimal tiene su PROPIO ancho, y se desplaza.**
    ///
    /// Sus filas son de 77 celdas: en un hueco partido o en un terminal
    /// estrecho la columna ASCII de la derecha no cabe, y negarle el eje
    /// horizontal la dejaba inalcanzable — exactamente la avería que este
    /// trabajo arregla en el texto.
    ///
    /// Y el tope depende del MODO, no del texto. Leer el ancho del texto en
    /// hexadecimal era además una barra que se movía sobre un contenido que
    /// no se movía: un fichero de texto con líneas de 200 columnas, puesto en
    /// hexadecimal, dibujaba barra y movía el pulgar sin que las filas
    /// cambiaran.
    #[test]
    fn el_hexadecimal_tiene_su_propio_ancho_y_se_desplaza() {
        let mut v = Viewer::new(vp(), b"\x00\x01payload".to_vec(), false);
        assert!(v.hex);
        assert_eq!(v.max_cols(), 77, "offset + 16 bytes + su columna ASCII");
        let entera = v.rows(1)[0].clone();
        assert!(entera.starts_with("00000000"));
        v.scroll_right(10);
        assert_eq!(v.hscroll(), 10);
        assert_eq!(
            v.rows(1)[0],
            entera[10..],
            "el volcado también se recorta por la izquierda"
        );

        // Un fichero de TEXTO ancho puesto en hexadecimal declara el ancho del
        // VOLCADO, no el del texto: si no, la barra prometía 200 columnas
        // sobre unas filas de 77.
        let ancho = format!("{}\n", "x".repeat(200)).into_bytes();
        let mut v = Viewer::new(vp(), ancho, false);
        assert_eq!(v.max_cols(), 200);
        v.scroll_right(40);
        v.toggle_hex();
        assert_eq!(v.max_cols(), 77);
        assert_eq!(
            v.hscroll(),
            0,
            "y la columna 40 del texto no es la 40 del volcado"
        );
    }

    /// **Un carácter ancho partido por el corte deja su hueco en blanco.**
    ///
    /// Media celda no se puede pintar, así que el carácter se va entero — pero
    /// tirarlo sin más corre esa fila una columna respecto a sus vecinas, y la
    /// rejilla es justo lo que un CSV o un log alineado necesitan del
    /// desplazamiento horizontal.
    #[test]
    fn un_caracter_ancho_partido_por_el_corte_deja_su_hueco() {
        let mut v = Viewer::new(vp(), "漢字x\nabcde\n".as_bytes().to_vec(), false);
        assert_eq!(v.max_cols(), 5, "dos ideogramas de dos celdas y una `x`");
        v.scroll_right(1);
        assert_eq!(
            v.rows(2),
            vec![" 字x", "bcde"],
            "el hueco del ideograma perdido mantiene las columnas enfrentadas"
        );
        v.scroll_right(1);
        assert_eq!(v.rows(2), vec!["字x", "cde"]);
    }

    /// Y con un tab por delante, el stop se cuenta por CELDAS: con `col += 1`
    /// por carácter, un ideograma antes de un tab movía el stop una columna y
    /// desalineaba el resto de la línea.
    #[test]
    fn el_tab_stop_se_cuenta_en_celdas_no_en_caracteres() {
        let v = Viewer::new(vp(), "漢\tx\n".as_bytes().to_vec(), false);
        assert_eq!(
            v.rows(1),
            vec!["漢      x"],
            "el ideograma ocupa DOS, así que faltan seis espacios hasta el 8"
        );
        assert_eq!(v.max_cols(), 9);
    }

    #[test]
    fn binario_no_imagen_cae_a_hexview_y_el_toggle_vuelve() {
        // Binario que NO es ninguna imagen reconocida → hexview automático.
        let bin = b"\x00\x01\x02\x03NUL\x00\x00payload".to_vec();
        let mut v = Viewer::new(vp(), bin, false);
        assert!(v.hex, "NUL sin BOM = hexview automático (spec §6)");
        assert!(!v.is_image(), "no es una imagen reconocida");
        let rows = v.rows(4);
        assert!(rows[0].starts_with("00000000"), "offset: {}", rows[0]);
        assert!(rows[0].contains("00 01 02 03"), "hex: {}", rows[0]);
        // Toggle manual: sale del hex (texto vacío en binario, pero es SU
        // decisión); x de nuevo vuelve.
        v.toggle_hex();
        assert!(!v.hex);
        v.toggle_hex();
        assert!(v.hex);
    }

    /// La cabecera declara el tamaño, y una que no se entiende se NIEGA.
    ///
    /// Negarse ante lo que no se entiende es la mitad del valor: el llamante
    /// usa esto para decidir si le da los bytes a un decodificador, y un
    /// `None` tratado como «adelante» sería la bomba de descompresión
    /// entrando por la puerta que existe para pararla.
    #[test]
    fn image_dimensions_lee_la_cabecera_y_niega_lo_que_no_entiende() {
        // PNG: IHDR en offset fijo.
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&1920u32.to_be_bytes());
        png.extend_from_slice(&1080u32.to_be_bytes());
        assert_eq!(image_dimensions(&png), Some((1920, 1080)));

        // GIF: little-endian tras la firma.
        let mut gif = b"GIF89a".to_vec();
        gif.extend_from_slice(&640u16.to_le_bytes());
        gif.extend_from_slice(&480u16.to_le_bytes());
        gif.extend_from_slice(&[0, 0]);
        assert_eq!(image_dimensions(&gif), Some((640, 480)));

        // BMP: alto NEGATIVO = filas de arriba abajo, no tamaño negativo.
        let mut bmp = b"BM".to_vec();
        bmp.resize(18, 0);
        bmp.extend_from_slice(&300i32.to_le_bytes());
        bmp.extend_from_slice(&(-200i32).to_le_bytes());
        assert_eq!(image_dimensions(&bmp), Some((300, 200)));

        // JPEG: hay que recorrer hasta el SOF0.
        let mut jpg = vec![0xFF, 0xD8];
        // Un APP0 que hay que saltar.
        jpg.extend_from_slice(&[0xFF, 0xE0, 0x00, 0x04, 0x00, 0x00]);
        jpg.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        jpg.extend_from_slice(&768u16.to_be_bytes());
        jpg.extend_from_slice(&1024u16.to_be_bytes());
        jpg.extend_from_slice(&[0; 8]);
        assert_eq!(image_dimensions(&jpg), Some((1024, 768)));

        // Lo que no se entiende se NIEGA, en vez de adivinar.
        assert_eq!(image_dimensions(b"no soy una imagen"), None);
        assert_eq!(
            image_dimensions(b"\x89PNG\r\n\x1a\n"),
            None,
            "una cabecera PNG incompleta no promete nada"
        );
        let mut sin_ihdr = b"\x89PNG\r\n\x1a\n".to_vec();
        sin_ihdr.extend_from_slice(&[0, 0, 0, 13]);
        sin_ihdr.extend_from_slice(b"iTXt");
        sin_ihdr.resize(32, 0);
        assert_eq!(
            image_dimensions(&sin_ihdr),
            None,
            "un PNG cuyo primer chunk no es IHDR no es uno que sepamos leer"
        );
        // Un JPEG cuyos segmentos no encajan: se abandona, no se da vueltas.
        assert_eq!(
            image_dimensions(&[0xFF, 0xD8, 0x00, 0x00, 0, 0, 0, 0, 0, 0, 0, 0]),
            None
        );
    }

    /// Una cabecera puede DECLARAR una imagen imposible, y eso es el ataque.
    #[test]
    fn una_cabecera_puede_declarar_una_bomba() {
        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        png.extend_from_slice(&[0, 0, 0, 13]);
        png.extend_from_slice(b"IHDR");
        png.extend_from_slice(&60000u32.to_be_bytes());
        png.extend_from_slice(&60000u32.to_be_bytes());
        let (w, h) = image_dimensions(&png).expect("la cabecera se lee");
        assert!(
            u64::from(w) * u64::from(h) > PIXEL_BUDGET,
            "36 gigapíxeles en 24 bytes de cabecera: es la bomba de \
             descompresión, y el presupuesto existe para verla antes de que \
             nadie decodifique"
        );
    }

    /// `image_format` reconoce cada formato soportado por bytes MÁGICOS y
    /// devuelve `None` en no-imágenes y en cabeceras truncadas.
    #[test]
    fn image_format_reconoce_por_bytes_magicos() {
        use super::{ImageFmt, image_format};
        assert_eq!(
            image_format(b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR"),
            Some(ImageFmt::Png)
        );
        assert_eq!(
            image_format(b"\xFF\xD8\xFF\xE0\x00\x10JFIF"),
            Some(ImageFmt::Jpeg)
        );
        assert_eq!(image_format(b"GIF87a\x01\x00"), Some(ImageFmt::Gif));
        assert_eq!(image_format(b"GIF89a\x01\x00"), Some(ImageFmt::Gif));
        assert_eq!(image_format(b"BM\x8a\x00\x00\x00"), Some(ImageFmt::Bmp));
        assert_eq!(
            image_format(b"RIFF\x24\x00\x00\x00WEBPVP8 "),
            Some(ImageFmt::Webp)
        );
        // No-imagen (texto plano) → None.
        assert_eq!(image_format(b"hola mundo\n"), None);
        // RIFF sin marca WEBP (p. ej. WAV) → None.
        assert_eq!(image_format(b"RIFF\x24\x00\x00\x00WAVEfmt "), None);
        // Cabecera PNG truncada (solo 4 bytes) → None: el prefijo no completa.
        assert_eq!(image_format(b"\x89PNG"), None);
        // RIFF truncado (<12 bytes) → None sin panic por slicing.
        assert_eq!(image_format(b"RIFF\x24\x00\x00\x00"), None);
    }

    /// Una imagen reconocida se marca como imagen (`is_image`/`image_kind`/
    /// `image_bytes` la exponen para que la GUI la pinte) SIN dejar de tener el
    /// hexview crudo disponible en `rows` (fallback de la TUI, que no pinta
    /// imágenes). «recargar como…» fuerza texto y sale del modo imagen.
    #[test]
    fn imagen_reconocida_se_marca_y_conserva_el_hex_de_fallback() {
        use super::ImageFmt;
        let png = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR".to_vec();
        let mut v = Viewer::new(vp(), png.clone(), false);
        assert!(v.is_image(), "PNG → imagen");
        assert_eq!(v.image_kind(), Some(ImageFmt::Png));
        assert_eq!(v.image_bytes(), Some(png.as_slice()));
        // El hexview crudo sigue disponible para frontends sin render (TUI).
        assert!(v.hex, "hexview de fallback activo");
        assert!(
            v.rows(1)[0].contains("89 50 4e 47"),
            "fallback hex disponible"
        );
        // «recargar como…» fuerza texto: sale del modo imagen.
        v.cycle_encoding();
        assert!(!v.is_image(), "forzado a texto → no imagen");
        assert_eq!(v.image_kind(), None);
        assert_eq!(v.image_bytes(), None);
        v.reset_encoding();
        assert!(v.is_image(), "reset vuelve a detección → imagen");
    }

    /// Una imagen TRUNCADA sigue reconociéndose por su cabecera (los bytes
    /// mágicos van al principio): `is_image` y `truncated` a la vez — la GUI
    /// decide si el decode parcial sale o cae a «imagen ilegible».
    #[test]
    fn imagen_truncada_se_reconoce_por_la_cabecera() {
        let png_head = b"\x89PNG\r\n\x1a\n\x00\x00\x00\rIHDR\x00\x00".to_vec();
        let v = Viewer::new(vp(), png_head, true);
        assert!(v.is_image());
        assert!(v.truncated);
        assert!(v.image_bytes().is_some());
    }

    /// H4/H5 de la auditoría: tabs EXPANDIDOS (ratatui los borraría) y ESC
    /// enmascarado — jamás alteración sin marca.
    #[test]
    fn tabs_expandidos_y_controles_enmascarados() {
        let v = Viewer::new(vp(), b"all:\n\tcc -o x x.c\n".to_vec(), false);
        let rows = v.rows(3);
        assert_eq!(rows[0], "all:");
        assert_eq!(rows[1], "        cc -o x x.c", "tab → 8 espacios");
        let v = Viewer::new(vp(), b"rojo:\x1b[31mX\n".to_vec(), false);
        assert_eq!(
            v.rows(2)[0],
            "rojo:\u{FFFD}[31mX",
            "ESC visible como \u{FFFD}"
        );
    }

    /// Trojan Source (CVE-2021-42574): un archivo de TEXTO UTF-8 válido con RLO
    /// (U+202E), isolate (U+2066), ZWSP (U+200B) y separador Zl (U+2028) — el
    /// viewer de texto debe enmascararlos a `�`, no dejarlos pasar (`is_control`
    /// solo cubría C0/C1; ahora `is_terminal_hazard`). GPUI reordena bidi.
    #[test]
    fn viewer_de_texto_no_pinta_bidi_ni_invisibles_crudos() {
        let hostile = "aguja \u{202E}reovni\u{2066} z\u{200B}w\u{2028}fin"
            .as_bytes()
            .to_vec();
        let v = Viewer::new(vp(), hostile, false);
        assert!(!v.hex, "es texto UTF-8, no binario");
        assert!(
            !v.rows(8)
                .iter()
                .flat_map(|r| r.chars())
                .any(norte_encoding::is_terminal_hazard),
            "el viewer de texto no puede pintar bidi/invisibles crudos"
        );
    }

    /// H7: togglear a hex con scroll alto reclampa (jamás pantalla en
    /// blanco).
    #[test]
    fn toggle_hex_reclampa_el_scroll() {
        use std::fmt::Write;
        let mut texto = String::new();
        for i in 0..100 {
            let _ = writeln!(texto, "{i}");
        }
        let mut v = Viewer::new(vp(), texto.into_bytes(), false);
        v.scroll_bottom();
        assert_eq!(v.scroll, 99);
        v.toggle_hex();
        assert!(v.scroll < v.total_rows(), "reclampado: {}", v.scroll);
        assert!(!v.rows(5).is_empty(), "el hexview pinta algo");
    }

    /// M4-P5: en modo preview de plugin, `rows()` pinta la salida del plugin
    /// (no la vista cruda), `preview_plugin()` da el nombre, y el output
    /// —texto de un TERCERO— sale ENMASCARADO (controles → `�`, jamás byte
    /// crudo).
    #[test]
    fn preview_de_plugin_reemplaza_la_vista_y_enmascara() {
        let v = Viewer::with_plugin_preview(
            vp(),
            "Markdown".to_owned(),
            "linea uno\nlinea\u{7}dos\nlinea tres",
            false,
        );
        assert_eq!(v.preview_plugin(), Some("Markdown"));
        assert!(!v.preview_lossy(), "no lossy");
        assert_eq!(v.total_rows(), 3, "3 líneas partidas por \\n");
        let rows = v.rows(10);
        assert_eq!(rows[0], "linea uno");
        assert_eq!(rows[2], "linea tres");
        assert_eq!(
            rows[1], "linea\u{FFFD}dos",
            "el control \\u{{7}} del plugin sale enmascarado, no crudo"
        );
        assert!(
            !rows[1].contains('\u{7}'),
            "jamás el byte de control crudo: {:?}",
            rows[1]
        );
    }

    /// #101: el flag `lossy` del wire llega a `preview_lossy()` en ambos
    /// constructores (plano y con estilo), para que el frontend pinte el aviso.
    #[test]
    fn preview_lossy_se_propaga_desde_el_wire() {
        let plano = Viewer::with_plugin_preview(vp(), "P".to_owned(), "a\u{FFFD}b", true);
        assert!(plano.preview_lossy(), "plano lossy");
        let styled = Viewer::with_plugin_preview_styled(vp(), "P".to_owned(), &[], true);
        assert!(styled.preview_lossy(), "styled lossy");
        // La vista cruda (sin preview) jamás reporta lossy por esta vía.
        let crudo = Viewer::new(vp(), b"hola".to_vec(), false);
        assert!(!crudo.preview_lossy(), "vista cruda: preview_lossy = false");
    }

    /// G3a (ADR 0037): `with_plugin_preview_styled` enmascara el `text` de
    /// CADA span igual que la ruta ANSI (mismo `crate::display_name`, sin
    /// una copia paralela), incluidos hostiles bidi (RLO) — jamás el byte
    /// crudo llega a `plugin_styled_rows`.
    #[test]
    fn preview_styled_enmascara_cada_span_bidi_incluido() {
        use norte_proto::methods::SpanWire;
        let lines = vec![
            vec![SpanWire {
                text: "buen\u{7}o".to_owned(), // BEL crudo
                role: None,
                fg: None,
                bg: None,
            }],
            vec![SpanWire {
                text: "a\u{202E}b".to_owned(), // RLO (bidi hostil)
                role: None,
                fg: None,
                bg: None,
            }],
        ];
        let v = Viewer::with_plugin_preview_styled(vp(), "Demo".to_owned(), &lines, false);
        assert_eq!(v.preview_plugin(), Some("Demo"));
        let rows = v.plugin_styled_rows(10).expect("modo preview con estilo");
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0][0].text, "buen\u{FFFD}o", "BEL enmascarado");
        assert!(!rows[0][0].text.contains('\u{7}'), "jamás el BEL crudo");
        assert!(
            !rows[1][0].text.contains('\u{202E}'),
            "jamás el RLO crudo: {:?}",
            rows[1][0].text
        );
    }

    /// G3a: `role` YA VALIDADO contra `norte_theme::Role` en la conversión.
    /// Un nombre reconocido (kebab-case) resuelve al `Role`; uno DESCONOCIDO
    /// (p. ej. el `"number"`/`"keyword"` del mini-highlighter de
    /// `previewer-demo`, que NO son `Role`s válidos a propósito — ver su
    /// rustdoc) colapsa a `None`, nunca panica ni deja pasar la cadena
    /// cruda. `fg` viaja SIEMPRE tal cual (es el fallback crudo, no algo
    /// que validar contra un conjunto cerrado).
    #[test]
    fn preview_styled_valida_role_desconocido_a_none() {
        use norte_proto::methods::SpanWire;
        let lines = vec![vec![
            SpanWire {
                text: "42".to_owned(),
                role: Some("number".to_owned()), // no es un Role válido
                fg: None,
                bg: None,
            },
            SpanWire {
                text: "TODO".to_owned(),
                role: Some("keyword".to_owned()), // tampoco
                fg: Some([255, 200, 0]),
                bg: Some([0, 0, 64]),
            },
            SpanWire {
                text: "err".to_owned(),
                role: Some("hostile-badge".to_owned()), // SÍ es un Role válido
                fg: None,
                bg: None,
            },
        ]];
        let v = Viewer::with_plugin_preview_styled(vp(), "Demo".to_owned(), &lines, false);
        let rows = v.plugin_styled_rows(10).expect("modo preview con estilo");
        assert_eq!(
            rows[0][0].role, None,
            "role desconocido → None, jamás panic"
        );
        assert_eq!(rows[0][1].role, None, "role desconocido → None");
        assert_eq!(
            rows[0][1].fg,
            Some((255, 200, 0)),
            "fg viaja tal cual, sin validar (no es un Role)"
        );
        assert_eq!(
            rows[0][2].role,
            Some(norte_theme::Role::HostileBadge),
            "role reconocido resuelve al Role"
        );
    }

    /// M4-P5: el scroll opera sobre las líneas del preview (topes
    /// incluidos).
    #[test]
    fn preview_de_plugin_scrollea_sobre_sus_lineas() {
        use std::fmt::Write;
        let mut out = String::new();
        for i in 0..20 {
            let _ = writeln!(out, "l{i}");
        }
        let mut v = Viewer::with_plugin_preview(vp(), "P".to_owned(), out.trim_end(), false);
        assert_eq!(v.total_rows(), 20);
        v.scroll_bottom();
        assert_eq!(v.scroll, 19);
        v.scroll_down(5);
        assert_eq!(v.scroll, 19, "tope inferior en el preview");
        v.scroll_top();
        assert_eq!(v.rows(2), vec!["l0", "l1"]);
    }

    #[test]
    fn cycle_y_reset_marcan_forzado_round_trip() {
        let mut v = Viewer::new(
            VPath::parse("mem:///a.txt").unwrap(),
            b"hola\n".to_vec(),
            false,
        );
        assert!(!v.is_forced());
        v.cycle_encoding(); // «recargar como…» → encoding forzado
        assert!(v.is_forced());
        v.reset_encoding(); // vuelve a detección automática
        assert!(!v.is_forced());
    }

    #[test]
    fn cr_de_mac_clasico_parte_lineas() {
        // CR solo (Mac clásico) parte línea igual que LF: 3 líneas → 3 filas.
        let v = Viewer::new(
            VPath::parse("mem:///a.txt").unwrap(),
            b"a\rb\rc".to_vec(),
            false,
        );
        assert!(!v.hex);
        assert_eq!(v.total_rows(), 3);
    }

    #[test]
    fn getters_exponen_el_estado_para_el_status_del_frontend() {
        let v = Viewer::new(
            VPath::parse("mem:///a.txt").unwrap(),
            b"hola\n".to_vec(),
            false,
        );
        assert_eq!(v.encoding_name(), "UTF-8");
        assert!(!v.is_forced());
        assert!(!v.had_errors());
        assert!(!v.hex);
    }
}
