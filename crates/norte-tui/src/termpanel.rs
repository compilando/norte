//! El panel de terminal (#362): un shell DENTRO de un hueco de la disposición.
//!
//! Es lo que Krusader tiene y norte no tenía. Lo que ya había es distinto y en
//! una cosa mejor: `app.toggle-panels` cede la terminal ENTERA a un subshell
//! vivo (ADR 0084) y `app.terminal` lanza un shell aparte. Lo que faltaba es
//! verlo A LA VEZ que los listados.
//!
//! # El reparto
//!
//! La emulación —los bytes a una rejilla de celdas— y el pty viven en
//! `norte-term`: la primera siempre, el segundo tras su feature. Aquí queda
//! solo el PINTADO con `ratatui`, que es lo único que no comparte con la
//! ventana. El día que la ventana pinte su panel usará el mismo shell y la
//! misma rejilla, así que los dos frontends enseñan lo mismo por construcción
//! y no porque alguien compare dos emuladores.
//!
//! # Quién manda en el teclado
//!
//! Éste es el único panel que consume BYTES y no comandos del catálogo, así
//! que mientras tiene las teclas se queda también con los acordes que serían
//! de norte. La salida es UN acorde suelto —el mismo `layout.terminal` que lo
//! abrió— y lo reconoce [`crate::keys`] antes de reenviar nada. Si el preset
//! lo atara a una secuencia, `Effective::lone_chord` devuelve `None` y el
//! panel NO toma las teclas: mejor un panel que se mira que uno del que no se
//! puede salir.
//!
//! # Lo que este panel NO hace todavía
//!
//! No instala el gancho del prompt, así que el listado no sigue al shell ni el
//! shell al listado: para eso está el subshell de #142, que sí lo instala. Un
//! panel que tecleara `cd` en el shell del lector tiene los problemas de #363
//! —y los tendría en un sitio donde el lector ve lo que pasa—, así que eso
//! espera a que ese agujero esté cerrado.

use norte_term::{ColorTerm, Estilo, Pantalla};

/// El shell del panel, con su rejilla. Es el de `norte-term`.
pub use norte_term::pty::Shell as TermPanel;

/// El id del kind, que es también el sufijo de su comando.
pub const KIND: &str = "terminal";

/// El comando que abre el panel, le da el teclado y se lo quita.
///
/// Es el MISMO que sale, y por eso está aquí y no escrito a mano en los dos
/// sitios que lo buscan en el keymap: el acorde que lo corre es el único que
/// el panel no le pasa al shell.
pub const COMANDO: &str = "layout.terminal";

/// Cómo se arranca el shell de un panel, con lo que norte decide.
///
/// El programa y el entorno los pone AQUÍ y no `norte-term`: resolver el shell
/// del lector y el contrato de `NORTE_LEVEL` son reglas de norte, no de un
/// emulador.
///
/// # Errors
/// Lo que falle al abrir el pty o al lanzar el shell.
pub fn abrir(dir: &std::path::Path, tam: (u16, u16)) -> std::io::Result<TermPanel> {
    norte_term::pty::Shell::abrir(
        &norte_term::pty::Arranque {
            // `login_shell` se niega a devolver un `$SHELL` relativo y cae a
            // `/bin/sh` (#302): sin eso, `portable_pty` lo buscaría por el
            // `cwd`, que aquí es el directorio que el lector está mirando.
            programa: &norte_frontend::shell::login_shell(),
            dir,
            tam,
            // El hijo sabe que está DENTRO de norte, igual que el subshell y
            // que una suspensión: mismo contrato de `NORTE_LEVEL`, y el prompt
            // del lector lo lee para decirlo.
            env: &[(
                norte_frontend::shell::LEVEL_VAR.into(),
                norte_frontend::shell::next_norte_level().into(),
            )],
        },
        // La misma tabla que contesta el subshell: una consulta de terminal se
        // contesta igual venga de donde venga.
        norte_frontend::subshell::terminal_reply,
    )
}

/// Las filas de la rejilla como spans de `ratatui`.
///
/// El troceado —dónde se corta una fila— lo hace `norte-term`, porque es la
/// misma decisión para los dos frontends y se toma una vez. Aquí solo se
/// traduce cada tramo al estilo de este toolkit.
///
/// El contenido es AJENO y aun así no se enmascara nada: lo que sale de la
/// rejilla ya no lleva ningún byte de control, y eso lo garantiza la rejilla,
/// no este código.
#[must_use]
pub fn filas<'a>(p: &Pantalla) -> Vec<ratatui::text::Line<'a>> {
    use ratatui::text::{Line, Span};
    let (_, alto) = p.tamano();
    (0..alto)
        .map(|f| {
            Line::from(
                p.fila_tramos(f)
                    .into_iter()
                    .map(|(texto, estilo)| Span::styled(texto, estilo_de(estilo)))
                    .collect::<Vec<Span<'a>>>(),
            )
        })
        .collect()
}

/// Un [`Estilo`] de terminal traducido al de `ratatui`.
///
/// Un color INDEXADO pasa tal cual (`Color::Indexed`): en una terminal lo
/// resuelve la paleta que el lector tiene puesta en su emulador, que es
/// exactamente lo que haría si el programa corriera fuera de norte.
///
/// **Por eso aquí no entra el tema de norte, ni como argumento.** Esto es
/// contenido de OTRO programa, no cromo nuestro, y un tema que le cambiara los
/// colores a un `ls --color` estaría mintiendo sobre lo que ese programa dijo.
/// Lo nuestro es el marco, y el marco lo pinta quien lo dibuja.
fn estilo_de(e: Estilo) -> ratatui::style::Style {
    use ratatui::style::{Modifier, Style};
    let mut s = Style::default();
    if let Some(c) = color_de(e.fg) {
        s = s.fg(c);
    }
    if let Some(c) = color_de(e.bg) {
        s = s.bg(c);
    }
    let mut m = Modifier::empty();
    if e.negrita {
        m |= Modifier::BOLD;
    }
    if e.tenue {
        m |= Modifier::DIM;
    }
    if e.cursiva {
        m |= Modifier::ITALIC;
    }
    if e.subrayado {
        m |= Modifier::UNDERLINED;
    }
    if e.inverso {
        m |= Modifier::REVERSED;
    }
    if e.tachado {
        m |= Modifier::CROSSED_OUT;
    }
    s.add_modifier(m)
}

fn color_de(c: ColorTerm) -> Option<ratatui::style::Color> {
    use ratatui::style::Color;
    match c {
        ColorTerm::PorDefecto => None,
        ColorTerm::Indexado(n) => Some(Color::Indexed(n)),
        ColorTerm::Rgb(r, g, b) => Some(Color::Rgb(r, g, b)),
    }
}

/// Dónde va el cursor dentro del área de contenido, si hay que pintarlo.
///
/// Devuelve `None` cuando el shell lo escondió (`CSI ?25l`, que es lo que hace
/// cualquier programa de pantalla completa) o cuando el panel no tiene las
/// teclas: un cursor parpadeando en un panel que no las tiene dice que el
/// teclado está ahí, y no lo está.
#[must_use]
pub fn cursor_en(
    p: &Pantalla,
    area: ratatui::layout::Rect,
    con_teclado: bool,
) -> Option<(u16, u16)> {
    if !con_teclado || !p.cursor_visible() {
        return None;
    }
    let (fila, col) = p.cursor();
    let (ancho, alto) = p.tamano();
    // La columna puede valer tanto como el ancho —el estado «pendiente de
    // salto»—, y ahí el cursor se pinta en la última celda: es donde un
    // terminal de verdad lo deja.
    let col = col.min(ancho.saturating_sub(1));
    (fila < alto).then(|| (area.x + col, area.y + fila))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El estilo de terminal llega a `ratatui` con los atributos puestos, y un
    /// índice sigue siendo un índice: resolverlo aquí le quitaría al emulador
    /// del lector su propia paleta.
    #[test]
    fn un_estilo_de_terminal_cruza_entero() {
        let e = Estilo {
            fg: ColorTerm::Indexado(4),
            bg: ColorTerm::Rgb(1, 2, 3),
            negrita: true,
            subrayado: true,
            ..Estilo::default()
        };
        let s = estilo_de(e);
        assert_eq!(s.fg, Some(ratatui::style::Color::Indexed(4)));
        assert_eq!(s.bg, Some(ratatui::style::Color::Rgb(1, 2, 3)));
        assert!(s.add_modifier.contains(ratatui::style::Modifier::BOLD));
        assert!(
            s.add_modifier
                .contains(ratatui::style::Modifier::UNDERLINED)
        );
    }

    /// Las celdas seguidas con el mismo estilo son UN span: ochenta spans por
    /// fila es lo que hace que un `make` en el panel se note en el resto.
    #[test]
    fn las_celdas_iguales_se_agrupan_en_un_span() {
        let mut p = Pantalla::nueva(10, 1);
        p.alimentar(b"aaa\x1b[31mbbb");
        let filas = filas(&p);
        let spans = &filas[0].spans;
        // Tres y no dos: detrás de `bbb` quedan cuatro celdas sin escribir, y
        // ésas llevan el estilo POR DEFECTO, no el rojo. Agruparlas con lo
        // rojo pintaría el fondo del resto de la línea del color de la última
        // orden, que es el fallo clásico de un emulador escrito a ojo.
        assert_eq!(spans.len(), 3, "dos estilos y el relleno: {spans:?}");
        assert_eq!(spans[0].content, "aaa");
        assert_eq!(spans[1].content, "bbb");
        assert_eq!(spans[2].content, "    ");
        assert_eq!(spans[2].style, ratatui::style::Style::default());
    }

    /// Sin teclado no se pinta cursor: sería decir que el teclado está aquí.
    #[test]
    fn el_cursor_solo_se_pinta_con_el_teclado_dentro() {
        let p = Pantalla::nueva(10, 3);
        let area = ratatui::layout::Rect::new(5, 2, 10, 3);
        assert_eq!(cursor_en(&p, area, false), None);
        assert_eq!(cursor_en(&p, area, true), Some((5, 2)));
    }

    /// Y tampoco cuando el shell lo esconde, que es lo que hace cualquier
    /// programa de pantalla completa mientras pinta.
    #[test]
    fn un_cursor_escondido_no_se_pinta() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"\x1b[?25l");
        let area = ratatui::layout::Rect::new(0, 0, 10, 3);
        assert_eq!(cursor_en(&p, area, true), None);
    }
}
