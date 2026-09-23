//! El panel de terminal en la ventana (#362, puente 95).
//!
//! Dos mitades. El SHELL y la rejilla son de `norte-term`, los mismos que usa
//! la terminal: el mismo pty, el mismo hilo lector y la misma emulación, así
//! que los dos frontends enseñan lo mismo por construcción y no porque alguien
//! compare dos emuladores. Y la TRADUCCIÓN de esa rejilla a lo que cruza el
//! puente, que es lo único propio de aquí.
//!
//! # Lo que NO se hace en la traducción, y es la decisión
//!
//! **Un color indexado no se resuelve.** El shell dice «color 4»; qué azul es
//! eso lo decide la paleta de quien pinta. Si se resolviera aquí a un
//! `#rrggbb`, el panel dejaría de obedecer al tema del lector y no habría
//! forma de arreglarlo desde el tema. Por eso [`TerminalColorView`] conserva
//! los dos casos distintos.
//!
//! **No se enmascara nada.** Lo que sale de la rejilla ya no puede llevar un
//! byte de control: el parser se come los escapes y tira los C0 que no mueven
//! el cursor. El enmascarado de nombres hostiles existe porque un nombre llega
//! crudo; esto no llega crudo, llega parseado.

use std::sync::Arc;

use norte_term::{ColorTerm, Estilo, Pantalla};
use tokio::sync::mpsc;

use crate::backend::HostBackend;
use crate::bridge::BridgeEnvelope;
use crate::dto::{TerminalColorView, TerminalSlotView, TerminalSpanView, UiUpdate};

use super::{ActionAck, Estado, Mensaje};

/// El kind del panel, que es también el sufijo de su comando.
pub(super) const KIND: &str = "terminal";

/// El comando que abre el panel y el que lo saca: es el MISMO.
pub(super) const COMANDO: &str = "layout.terminal";

impl Estado {
    /// Abre el panel de terminal, o lo pone delante.
    ///
    /// **Nunca lo cierra**, al revés que `alternar_hueco`: dentro hay un shell
    /// del lector con lo que tuviera a medias, y cerrarlo lo mata. Cerrar es
    /// `layout.close-slot`, que se llama como lo que hace.
    pub(super) fn abrir_panel_de_terminal(
        &mut self,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> (ActionAck, Vec<BridgeEnvelope<UiUpdate>>) {
        if let Some(id) = self.hueco_de_kind(KIND) {
            // Detrás de otra pestaña: se pone delante.
            let detras = self
                .arbol
                .tabs_of(id)
                .is_some_and(|(t, a)| t.get(a) != Some(&id));
            if detras {
                return self.elegir_pestana(id.0, backend, buzon);
            }
            // Ya delante, y aquí está la PUERTA, en las dos direcciones. La
            // tecla es una sola, así que tiene que llevar y traer:
            //
            // - con el foco fuera, se lo damos (y sin esto abrir por la tecla
            //   nunca enfocaba el panel: `abrir_hueco_de_kind` deja el foco en
            //   el listado recordado, así que solo se entraba con el ratón o
            //   con el anillo);
            // - con el foco DENTRO, se devuelve al listado. Sin esto la salida
            //   documentada no hacía nada y el panel era una ratonera: ahí
            //   dentro todas las teclas son del shell, incluida la del anillo.
            //
            // Lo que NO hace, y es la diferencia con `alternar_hueco`: cerrar.
            // Dentro hay un shell del lector.
            let dentro = self
                .roles
                .get(norte_frontend::layout::RoleId::Active)
                .is_some_and(|a| a == id);
            let destino = if dentro {
                norte_frontend::layout::SlotId(self.activo())
            } else {
                id
            };
            self.roles
                .set(norte_frontend::layout::RoleId::Active, destino);
            self.reconcilia_roles();
            // Y si el hueco existe SIN shell, se arranca aquí. Es el caso de
            // la sesión restaurada —el árbol se guarda, el shell no— y el del
            // arranque que falló: sin esto el panel quedaba muerto para toda
            // la vida de la ventana, porque esta rama volvía antes de mirar.
            if !dentro && self.terminal.is_none() {
                self.arrancar_si_falta(buzon);
            }
            let snap = self.snapshot();
            let sobre = self.sobre(UiUpdate::Snapshot(Box::new(snap)));
            return (self.aplicada(), vec![sobre]);
        }
        // Un shell se sienta en un directorio del sistema de ficheros: sobre
        // un `sftp://` no hay dónde sentarlo, y abrirlo en el `$HOME` sin
        // decir nada sería abrirlo en otro sitio. Es la misma puerta que
        // `app.terminal`, con el mismo motivo y la misma frase.
        let dir = self.hueco().pane.dir().clone();
        if !norte_frontend::shell::is_local(&dir) {
            let fuera = self.decir("host-not-local");
            return (
                ActionAck::Unavailable {
                    reason_key: "host-not-local".to_owned(),
                },
                fuera,
            );
        }
        let (ack, mut updates) = self.abrir_hueco_de_kind(KIND, backend, buzon);
        // El foco va al panel: abrirlo y no poder teclear dentro sin buscar el
        // ratón no es abrirlo. `abrir_hueco_de_kind` lo deja en el listado
        // recordado, así que se pone aquí.
        if let Some(id) = self.hueco_de_kind(KIND) {
            self.roles.set(norte_frontend::layout::RoleId::Active, id);
            self.reconcilia_roles();
        }
        updates.extend(self.arrancar_si_falta(buzon));
        (ack, updates)
    }

    /// Arranca el shell si el hueco existe y no lo tiene, y republica.
    ///
    /// Lo llaman los dos caminos —abrir el hueco y volver a él— porque el
    /// segundo es el de la sesión restaurada: el árbol se guarda y el shell
    /// no, así que al arrancar la ventana hay hueco sin shell.
    fn arrancar_si_falta(
        &mut self,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if self.terminal.is_some() || self.hueco_de_kind(KIND).is_none() {
            return Vec::new();
        }
        let dir = self.hueco().pane.dir().clone();
        match arrancar(&dir) {
            Ok(shell) => {
                // Se deja constancia, como hace la terminal y con el mismo «no
                // va al diario» escrito: un shell que abre el lector es el
                // lector actuando con sus permisos, no una mutación de norte.
                // Pero arrancar un shell es lo de más privilegio que hace un
                // frontend, y el panel de registro es ahora una superficie que
                // se mira también aquí.
                tracing::info!(
                    "la ventana abrió un shell en un panel de terminal \
                     (no va al diario: sin actor y sin reversa)"
                );
                self.terminal = Some(shell);
                self.sondear_terminal(buzon);
            }
            Err(e) => {
                // Y se DICE, no solo al log: el hueco se queda, así que sin
                // una frase el lector ve un panel vacío sin saber por qué.
                tracing::warn!(error = %e, "no se pudo abrir el shell del panel");
                return self.decir("host-shell-failed");
            }
        }
        self.republicar_terminal()
    }

    /// Programa el siguiente tic del panel, si sigue habiendo panel.
    ///
    /// Se rearma solo mientras el hueco siga en el árbol y se apaga al
    /// cerrarlo — mismo mecanismo que el sondeo del registro, y por el mismo
    /// motivo: un temporizador de 30 Hz que sobreviviera al panel estaría
    /// despertando al actor para no pintar nada.
    fn sondear_terminal(&self, buzon: &mpsc::Sender<Mensaje>) {
        if self.hueco_de_kind(KIND).is_none() {
            return;
        }
        let (buzon, epoca) = (buzon.clone(), self.terminal_epoca);
        tokio::spawn(async move {
            tokio::time::sleep(super::TERMINAL_TIC).await;
            let _ = buzon.send(Mensaje::TerminalTic(epoca)).await;
        });
    }

    /// Vuelca lo que el shell escribió y republica si cambió algo.
    ///
    /// Un tic sin bytes no produce parche: un shell quieto no despierta al
    /// renderer treinta veces por segundo.
    pub(super) fn terminal_tic(
        &mut self,
        epoca: u64,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Vec<BridgeEnvelope<UiUpdate>> {
        if epoca != self.terminal_epoca {
            // De una apertura anterior: se deja morir sin rearmar.
            return Vec::new();
        }
        // Si el hueco ya no está, el shell se va con él y el tic no se rearma.
        if self.hueco_de_kind(KIND).is_none() {
            self.soltar_terminal();
            return Vec::new();
        }
        self.sondear_terminal(buzon);
        // El tamaño del hueco, ANTES de bombear: el pty tiene que saberlo o un
        // programa de pantalla completa pinta para un ancho que no es el suyo
        // y el ajuste de línea sale mal. Se arrancó en 80x24 y nadie lo movía.
        //
        // Del REPARTO, igual que el visor acoplado saca su alto: menos el
        // marco, que lo pinta el renderer. `redimensionar` no hace nada si no
        // cambió, así que preguntarlo en cada tic es gratis.
        let tam = self.tamano_del_terminal();
        let Some(t) = self.terminal.as_mut() else {
            return Vec::new();
        };
        if let Some(tam) = tam {
            t.redimensionar(tam);
        }
        let cambio = t.bombear();
        // Si el shell se fue, el hueco lo DICE en vez de enseñar la última
        // pantalla de un proceso que ya no existe. El hueco se queda: cerrarlo
        // por su cuenta movería la disposición de alguien sin que la tocara.
        if t.muerto() {
            // `soltar_terminal` y no `self.terminal = None`: sube la época, y
            // sin eso el tic que ya se rearmó arriba seguiría girando a 30 Hz
            // para siempre sobre un panel sin shell — el «gira en reposo» otra
            // vez, esta vez disparado por que el shell se fue.
            self.soltar_terminal();
            return self.republicar_terminal();
        }
        if cambio {
            return self.republicar_terminal();
        }
        Vec::new()
    }

    /// Suelta el shell y para su bomba. Lo llama el cierre del hueco.
    pub(super) fn soltar_terminal(&mut self) {
        // El `Drop` de `Shell` mata el shell y lo espera.
        self.terminal = None;
        // Y la época sube: el tic en vuelo se deja morir sin rearmarse.
        self.terminal_epoca += 1;
    }

    /// Las teclas cuando el panel de terminal tiene el foco: TODAS al shell,
    /// menos la que saca.
    ///
    /// `None` = este panel no las quiere, y la tecla sigue su camino normal.
    ///
    /// No se parece a `tecla_en_preview` y no debería: aquél resuelve contra
    /// un keymap, y aquí no hay keymap que valga — dentro de un shell `tab`,
    /// las flechas y `ctrl+c` significan lo que el shell diga. Lo único que
    /// norte se queda es el acorde suelto que abrió el panel; si el preset lo
    /// ata a una secuencia no hay puerta, y entonces el panel NO toma las
    /// teclas, que es mejor que un panel del que no se puede salir.
    pub(super) fn tecla_en_terminal(
        &mut self,
        k: &crate::keys::KeyInput,
        backend: &Arc<dyn HostBackend>,
        buzon: &mpsc::Sender<Mensaje>,
    ) -> Option<(ActionAck, Vec<BridgeEnvelope<UiUpdate>>)> {
        let salida = self.acorde_de_salida()?;
        let foco = self.roles.get(norte_frontend::layout::RoleId::Active)?;
        if super::kind_de(&self.arbol, foco).is_none_or(|k| k.as_str() != KIND) {
            return None;
        }
        let chord = k.to_chord().ok()?;
        if chord == salida {
            // El MISMO camino que lo abrió: la tecla es una, así que el
            // regreso tiene que ser el mismo código.
            return Some(self.abrir_panel_de_terminal(backend, buzon));
        }
        let bytes = norte_frontend::subshell::chord_a_bytes(chord)?;
        self.terminal_escribir(&bytes);
        Some((self.aplicada(), Vec::new()))
    }

    /// El tamaño del hueco del terminal en celdas, sin el marco.
    ///
    /// `None` si el hueco no está colocado —detrás de una pestaña, o no cabe—:
    /// entonces no se toca el pty, porque el último tamaño bueno es mejor que
    /// uno inventado.
    fn tamano_del_terminal(&self) -> Option<(u16, u16)> {
        let id = self.hueco_de_kind(KIND)?;
        let (_, r) = self.reparto.placements.iter().find(|(s, _)| *s == id)?;
        Some((r.width.saturating_sub(2), r.height.saturating_sub(2)))
    }

    /// El acorde SUELTO que corre `layout.terminal`, si el preset da uno.
    ///
    /// La regla de que tenga que ser suelto vive en `Effective::lone_chord` y
    /// la comparten los dos sitios que ceden el teclado entero a otro
    /// programa: el subshell de la terminal y este panel. Una secuencia de dos
    /// obligaría a robarle al shell su primera tecla justo donde el lector la
    /// está escribiendo.
    fn acorde_de_salida(&self) -> Option<norte_frontend::keymap::Chord> {
        self.efectivo.lone_chord(COMANDO)
    }

    /// Le manda bytes al shell, si lo hay.
    pub(super) fn terminal_escribir(&mut self, bytes: &[u8]) {
        if let Some(t) = self.terminal.as_mut() {
            t.escribir(bytes);
        }
    }

    /// La foto entera, que es como republica cualquier hueco de esta ventana.
    fn republicar_terminal(&mut self) -> Vec<BridgeEnvelope<UiUpdate>> {
        let snap = self.snapshot();
        vec![self.sobre(UiUpdate::Snapshot(Box::new(snap)))]
    }

    /// La vista del panel para la foto, si el hueco existe.
    pub(super) fn panel_de_terminal(&self, slot: u32) -> TerminalSlotView {
        vista(
            slot,
            self.terminal.as_ref().map(norte_term::pty::Shell::pantalla),
        )
    }
}

/// Arranca el shell con lo que decide norte: el programa y el entorno.
///
/// Van aquí y no en `norte-term` porque resolver el shell del lector y el
/// contrato de `NORTE_LEVEL` son reglas de norte, no de un emulador.
fn arrancar(dir: &norte_proto::VPath) -> std::io::Result<norte_term::pty::Shell> {
    let nativo = norte_vfs::native::vpath_to_native(dir)
        .map_err(|_| std::io::Error::other("el directorio no es una ruta nativa"))?;
    norte_term::pty::Shell::abrir(
        &norte_term::pty::Arranque {
            // `login_shell` se niega a devolver un `$SHELL` relativo y cae a
            // `/bin/sh` (#302): sin eso se buscaría por el `cwd`, que aquí es
            // el directorio que el lector está mirando.
            programa: &norte_frontend::shell::login_shell(),
            dir: &nativo,
            // El tamaño de verdad lo pone el renderer cuando dice qué hueco le
            // tocó; éste es el de arranque.
            tam: (80, 24),
            env: &[(
                norte_frontend::shell::LEVEL_VAR.into(),
                norte_frontend::shell::next_norte_level().into(),
            )],
        },
        norte_frontend::subshell::terminal_reply,
    )
}

/// La rejilla de un shell, pasada a la vista del puente.
///
/// Sin rejilla —no hay shell— la vista lo DICE: un panel en blanco y un panel
/// sin shell se ven igual y no son lo mismo.
#[must_use]
fn vista(slot_id: u32, pantalla: Option<&Pantalla>) -> TerminalSlotView {
    let Some(p) = pantalla else {
        return TerminalSlotView {
            slot_id,
            rows: Vec::new(),
            cursor: None,
            no_shell: true,
        };
    };
    let (_, alto) = p.tamano();
    TerminalSlotView {
        slot_id,
        rows: (0..alto)
            .map(|f| {
                p.fila_tramos(f)
                    .into_iter()
                    .map(|(text, estilo)| span(text, estilo))
                    .collect()
            })
            .collect(),
        cursor: cursor(p),
        no_shell: false,
    }
}

/// Dónde va el cursor, o `None` si el shell lo escondió.
///
/// Lo esconde cualquier programa de pantalla completa mientras pinta, y
/// entonces pintarlo sería inventarse dónde está.
fn cursor(p: &Pantalla) -> Option<(u16, u16)> {
    if !p.cursor_visible() {
        return None;
    }
    let (fila, col) = p.cursor();
    let (ancho, alto) = p.tamano();
    // La columna puede valer tanto como el ancho —el estado «pendiente de
    // salto»— y ahí el cursor se pinta en la última celda, que es donde lo
    // deja un terminal de verdad.
    (fila < alto).then(|| (fila, col.min(ancho.saturating_sub(1))))
}

fn span(text: String, e: Estilo) -> TerminalSpanView {
    TerminalSpanView {
        text,
        fg: color(e.fg),
        bg: color(e.bg),
        bold: e.negrita,
        dim: e.tenue,
        italic: e.cursiva,
        underline: e.subrayado,
        reverse: e.inverso,
        strike: e.tachado,
    }
}

fn color(c: ColorTerm) -> Option<TerminalColorView> {
    match c {
        ColorTerm::PorDefecto => None,
        ColorTerm::Indexado(index) => Some(TerminalColorView::Indexed { index }),
        ColorTerm::Rgb(r, g, b) => Some(TerminalColorView::Rgb {
            hex: format!("#{r:02x}{g:02x}{b:02x}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Un índice cruza el puente SIN resolver, y un RGB como hex.
    ///
    /// Es la decisión del módulo, y se fija con un test porque el día que
    /// alguien «mejore» esto resolviendo el índice contra el tema, el panel
    /// dejará de obedecer al tema del lector y nada más se pondrá rojo.
    #[test]
    fn un_indice_sigue_siendo_un_indice_y_un_rgb_es_hex() {
        let mut p = Pantalla::nueva(12, 1);
        p.alimentar(b"\x1b[31ma\x1b[38;2;1;2;3mb");
        let v = vista(7, Some(&p));
        let fila = &v.rows[0];
        assert_eq!(
            fila[0].fg,
            Some(TerminalColorView::Indexed { index: 1 }),
            "el color 1 del shell no se resuelve aquí"
        );
        assert_eq!(
            fila[1].fg,
            Some(TerminalColorView::Rgb {
                hex: "#010203".to_owned()
            })
        );
    }

    /// Sin shell, la vista lo DICE en vez de mandar una rejilla vacía.
    #[test]
    fn sin_shell_se_dice() {
        let v = vista(3, None);
        assert!(v.no_shell);
        assert!(v.rows.is_empty());
        assert_eq!(v.cursor, None);
    }

    /// El cursor no viaja si el shell lo escondió.
    #[test]
    fn un_cursor_escondido_no_viaja() {
        let mut p = Pantalla::nueva(10, 3);
        p.alimentar(b"hola");
        assert_eq!(vista(1, Some(&p)).cursor, Some((0, 4)));
        p.alimentar(b"\x1b[?25l");
        assert_eq!(vista(1, Some(&p)).cursor, None);
    }

    /// Las filas van TODAS las que tiene la rejilla, incluidas las vacías: un
    /// terminal no se desplaza como una lista, se repinta, y un renderer que
    /// recibiera solo las escritas tendría que adivinar el alto.
    #[test]
    fn van_todas_las_filas() {
        let mut p = Pantalla::nueva(6, 4);
        p.alimentar(b"una");
        let v = vista(1, Some(&p));
        assert_eq!(v.rows.len(), 4);
    }
}
