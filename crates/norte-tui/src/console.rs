//! La terminal viva, en las DOS direcciones: lo que el lector teclea y lo que
//! se le pinta.
//!
//! Existía solo la primera mitad. Una espera larga —`cd` a un bucket remoto,
//! que puede tardar segundos— corría su propio `select!` leyendo teclas para
//! poder cancelarse, y en todo ese rato nadie repintaba: la pantalla se quedaba
//! con el último fotograma, que es indistinguible de un cuelgue. El arreglo no
//! es un indicador, es que quien espera pueda pintar; el indicador viene
//! después ([`norte_frontend::busy`]).
//!
//! Van juntos y no como dos parámetros por el mismo motivo que
//! `jobs::inflight` juntó diecisiete variables de `run`: quien espera
//! necesita exactamente estos dos, siempre los dos, y separarlos hace que cada
//! función del camino nazca con un parámetro más.

use crossterm::event::EventStream;

use crate::app::App;

/// El stream de eventos más con qué repintar mientras se espera.
pub struct Console<'a> {
    /// Lo que el lector teclea. Público: los `select!` lo usan directamente —
    /// un método `next_event(&mut self)` retendría `&mut self` durante todo el
    /// future y el brazo que repinta, en el mismo `select!`, no compilaría.
    pub events: &'a mut EventStream,
    paint: Paint<'a>,
    /// Ya se avisó de un fallo de repintado en esta consola.
    fallo_avisado: bool,
}

/// Con qué se pinta, si se puede pintar.
enum Paint<'a> {
    /// El bucle de eventos, que tiene la terminal.
    Terminal(&'a mut crate::tty::Tui),
    /// No hay con qué. EXPLÍCITO, y no un `Option` que alguien olvidó
    /// rellenar: un test sin terminal y un contexto que de verdad no puede
    /// pintar deben decirlo, no parecerse a un descuido.
    Detached,
}

impl<'a> Console<'a> {
    /// La consola del bucle: lee y pinta.
    pub fn new(events: &'a mut EventStream, terminal: &'a mut crate::tty::Tui) -> Self {
        Self {
            events,
            paint: Paint::Terminal(terminal),
            fallo_avisado: false,
        }
    }

    /// Una consola que solo lee (tests, y cualquier contexto sin terminal).
    pub fn detached(events: &'a mut EventStream) -> Self {
        Self {
            events,
            paint: Paint::Detached,
            fallo_avisado: false,
        }
    }

    /// La terminal, para quien la necesita para algo que no es repintar:
    /// arrancar un editor, suspenderse, medir la pantalla.
    ///
    /// Existe porque la terminal tiene UN dueño y ahora es esta consola: dos
    /// `&mut` a la vez no compilan, y un segundo parámetro `terminal` al lado
    /// de la consola sería justo el desdoble que este tipo vino a evitar.
    /// `None` en una consola desligada — y ahí, quien iba a lanzar algo, no
    /// lanza nada: es lo correcto, no una degradación (sin terminal no hay
    /// nada que ceder).
    pub fn terminal(&mut self) -> Option<&mut crate::tty::Tui> {
        match &mut self.paint {
            Paint::Terminal(t) => Some(t),
            Paint::Detached => None,
        }
    }

    /// Repinta con el estado actual, si hay con qué.
    ///
    /// EXENCIÓN puntual de la regla 2, la misma que el `draw` del bucle de
    /// eventos y por el mismo motivo (patrón async oficial de ratatui): el
    /// dibujo escribe la terminal de control de forma síncrona. Aquí hay UNA
    /// diferencia que conviene tener escrita: el bucle dibuja cuando no hay
    /// nada en vuelo, y esto dibuja mientras la ÚNICA vía de cancelación está
    /// pendiente. Si la terminal se atasca escribiendo (un XOFF, un pty remoto
    /// con el buffer lleno), el `select!` no avanza y `Esc` deja de responder
    /// mientras dure el atasco. Se acepta porque la alternativa —no repintar—
    /// es el fallo que esto viene a arreglar, y porque el bucle corre esa misma
    /// exposición en cada vuelta.
    ///
    /// Best-effort A PROPÓSITO: un fallo de dibujo no puede cambiar el tipo de
    /// retorno de una navegación (ni convertir «no pude pintar el spinner» en
    /// «la navegación falló»), y no se pierde nada — el `draw` del bucle, que
    /// sí es fatal, vuelve a intentarlo en cuanto la espera acaba.
    pub fn repaint(&mut self, app: &App) {
        if let Paint::Terminal(term) = &mut self.paint
            && let Err(e) = term.draw(|f| crate::ui::draw(f, app))
        {
            // Una vez por espera, no doce por segundo: una terminal rota
            // durante diez minutos son 7500 líneas idénticas que entierran lo
            // que sí importa en el log.
            if !self.fallo_avisado {
                self.fallo_avisado = true;
                tracing::warn!(error = %e, "no se pudo repintar durante una espera");
            }
        }
    }
}

/// Cómo acabó una espera pintada.
pub enum Waited<T> {
    /// El trabajo terminó.
    Done(T),
    /// El lector pulsó `Esc`.
    Cancelled,
    /// El lector pulsó `Ctrl+C`: cancelar Y salir.
    Quit,
}

/// Espera `fut` repintando el spinner y dejando cancelar.
///
/// Es el patrón de TODA espera larga del TUI, y está aquí en vez de repetido
/// porque repetirlo fue el fallo: cuando solo lo tenía la navegación, el
/// refresco de paneles y la apertura del visor seguían congelando la pantalla
/// exactamente igual, y con una consola en la mano que ya sabía pintar. Quien
/// añada la cuarta espera hereda el spinner por usar esto.
///
/// `Esc` y `Ctrl+C` son FIJOS aquí, no pasan por el keymap: son la salida de
/// emergencia y no deben poder remapearse a algo que no exista. El resto de
/// teclas se descarta mientras dura la espera.
///
/// El llamante pone `app.busy` ANTES y lo quita DESPUÉS; esto solo le va
/// poniendo al día lo transcurrido.
pub async fn wait_painting<T>(
    console: &mut Console<'_>,
    app: &mut App,
    started: std::time::Instant,
    fut: impl Future<Output = T>,
) -> Waited<T> {
    use futures::StreamExt as _;

    tokio::pin!(fut);
    // Al ritmo del spinner y no a otro: con dos constantes distintas, el
    // fotograma se salta o se repite y nadie se entera.
    let mut tick = tokio::time::interval(norte_frontend::busy::FRAME_EVERY);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            res = &mut fut => return Waited::Done(res),
            _ = tick.tick() => {
                if let Some(b) = &mut app.busy {
                    b.elapsed = started.elapsed();
                    // Antes del umbral no hay nada nuevo que enseñar: repintar
                    // un fotograma idéntico es trabajo que el lector paga.
                    if b.visible() {
                        console.repaint(app);
                    }
                }
            }
            maybe = console.events.next() => {
                match maybe {
                    Some(Ok(crossterm::event::Event::Key(key)))
                        if key.kind == crossterm::event::KeyEventKind::Press =>
                    {
                        use crossterm::event::{KeyCode, KeyModifiers};
                        match (key.code, key.modifiers) {
                            (KeyCode::Char('c'), m) if m.contains(KeyModifiers::CONTROL) => {
                                return Waited::Quit;
                            }
                            (KeyCode::Esc, _) => return Waited::Cancelled,
                            _ => {}
                        }
                    }
                    Some(Ok(_)) => {}
                    // El stream de eventos se acabó o se rompió: no hay quien
                    // cancele ni quien siga, así que se abandona la espera.
                    Some(Err(_)) | None => return Waited::Cancelled,
                }
            }
        }
    }
}
