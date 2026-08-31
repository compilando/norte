//! Qué está esperando el lector, y desde cuándo.
//!
//! El TUI ya sabía cancelar una navegación lenta (`Esc`) y no sabía DECIR que
//! estaba esperando: durante un `connect` a un bucket remoto la pantalla se
//! quedaba con el último fotograma, indistinguible de un cuelgue. Esto es el
//! dato que las superficies pintan, y vive aquí —y no en el TUI— porque la
//! ventana necesita exactamente el mismo, y una decisión de presentación
//! duplicada entre frontends diverge en silencio (ADR 0077).
//!
//! Dos reglas que el tipo hace cumplir, no el que pinta:
//!
//! - **Nada antes del umbral.** Un `cd` local tarda milisegundos; enseñar un
//!   destello en cada uno convierte el indicador en ruido, y el ruido se deja
//!   de mirar. [`Busy::visible`] contesta por todos.
//! - **Indeterminado, y honesto.** Aquí no hay porcentaje porque en un
//!   `connect` no existe: la fase es una red que contesta o no. El repositorio
//!   ya tiene la opinión escrita en `modal.rs` —«una etiqueta de "comprobando…"
//!   que no avanza nunca es una mentira con forma de spinner»—; el corolario es
//!   que una BARRA que se inventa el avance es peor.

use std::time::Duration;

use norte_proto::VPath;

/// Cuánto tiene que durar algo para merecer indicador.
///
/// 250 ms: por debajo, la operación termina antes de que el ojo la registre y
/// lo único que se ve es el parpadeo; por encima, aparece mucho antes de que a
/// nadie le dé tiempo a pensar que el programa se ha colgado.
pub const THRESHOLD: Duration = Duration::from_millis(250);

/// Fotogramas del spinner, los mismos que usa cargo.
///
/// Braille y no `|/-\`: es lo que un usuario de herramientas Rust ya reconoce
/// como «trabajando», y esta TUI ya depende de dibujo de cajas Unicode, así que
/// no añade un requisito de fuente que antes no hubiera.
const FRAMES: [char; 10] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

/// Cada cuánto avanza el spinner.
///
/// Pública porque quien espera tiene que despertar EXACTAMENTE a este ritmo:
/// con dos constantes de 80 ms en dos módulos, cambiar una salta o repite
/// fotogramas y nada se pone rojo.
pub const FRAME_EVERY: Duration = Duration::from_millis(80);

/// Qué clase de trabajo espera. Vocabulario CERRADO: cada variante tiene su
/// clave Fluent y [`BusyKind::key`] es total, así que no puede aparecer un
/// trabajo en vuelo sin etiqueta que decirle al lector.
/// Solo hay variantes para esperas que BLOQUEAN el repintado. La búsqueda, la
/// comparación, la sincronización y el plan del modelo NO están aquí a
/// propósito: viven en `jobs::inflight`, las cosecha el bucle principal, y ese
/// bucle ya repinta cada 100 ms — darles una variante sugeriría que aquí falta
/// cablearlas cuando lo que pasa es que no la necesitan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BusyKind {
    /// Estableciendo una conexión remota (el caso que destapó esto).
    Connecting,
    /// Listando un directorio.
    Listing,
    /// Trayendo un fichero para abrirlo en el visor.
    Opening,
}

impl BusyKind {
    /// La clave Fluent del verbo. Total por construcción.
    ///
    /// ```
    /// use norte_frontend::busy::BusyKind;
    /// assert_eq!(BusyKind::Connecting.key(), "busy-connecting");
    /// ```
    #[must_use]
    pub fn key(self) -> &'static str {
        match self {
            Self::Connecting => "busy-connecting",
            Self::Listing => "busy-listing",
            Self::Opening => "busy-opening",
        }
    }

    /// Todas las variantes, para los tests que exigen totalidad.
    #[must_use]
    pub const fn all() -> [Self; 3] {
        [Self::Connecting, Self::Listing, Self::Opening]
    }
}

/// Un trabajo en vuelo, tal y como se pinta.
///
/// No lleva `Instant`: lleva lo TRANSCURRIDO, que es lo que las dos preguntas
/// necesitan y lo único que un test puede fijar sin un reloj. Quien lo
/// construye (el bucle de eventos, que sí tiene el `Instant` de arranque) hace
/// la resta una vez.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Busy {
    /// Qué se está haciendo.
    pub kind: BusyKind,
    /// Sobre qué: el `VPath` CRUDO, no un texto ya renderizado.
    ///
    /// Esto se corrigió tras la auditoría de codificación, y el motivo vale
    /// para cualquier ruta que cruce un tipo compartido: renderizar aquí
    /// dejaba fuera de alcance las tres cosas que solo sabe quien pinta —el
    /// badge de nombre alterado, la reinterpretación de codificación del panel
    /// (#98/F2) y el ancho disponible—, y las tres se perdían a la vez. Lo que
    /// se veía era una ruta enmascarada SIN la marca que dice que se enmascaró,
    /// en la única superficie donde además se ofrece cancelar.
    pub target: Option<VPath>,
    /// El panel al que afecta, si afecta a uno. `None` = trabajo de sesión.
    pub pane: Option<usize>,
    /// Desde que empezó.
    pub elapsed: Duration,
}

impl Busy {
    /// Un trabajo recién nacido (`elapsed` cero): todavía invisible.
    #[must_use]
    pub fn new(kind: BusyKind, target: Option<VPath>, pane: Option<usize>) -> Self {
        Self {
            kind,
            target,
            pane,
            elapsed: Duration::ZERO,
        }
    }

    /// ¿Se pinta ya?
    ///
    /// La respuesta vive AQUÍ y no en cada superficie: dos sitios que decidan
    /// por su cuenta acaban con la cabecera girando y la barra callada.
    ///
    /// ```
    /// use norte_frontend::busy::{Busy, BusyKind, THRESHOLD};
    /// let mut b = Busy::new(BusyKind::Connecting, None, Some(0));
    /// assert!(!b.visible(), "recién nacida no se enseña");
    /// b.elapsed = THRESHOLD;
    /// assert!(b.visible());
    /// ```
    #[must_use]
    pub fn visible(&self) -> bool {
        self.elapsed >= THRESHOLD
    }

    /// El fotograma del spinner para lo transcurrido.
    ///
    /// Derivado del TIEMPO y no de un contador de repintados: así las dos
    /// superficies giran a la vez aunque una se pinte más veces que la otra, y
    /// un test puede fijarlo sin depender de cuántos fotogramas hubo.
    ///
    /// ```
    /// use norte_frontend::busy::{Busy, BusyKind, FRAME_EVERY};
    /// let mut b = Busy::new(BusyKind::Listing, None, Some(0));
    /// let primero = b.frame();
    /// b.elapsed = FRAME_EVERY;
    /// assert_ne!(primero, b.frame(), "un fotograma después ha girado");
    /// ```
    #[must_use]
    pub fn frame(&self) -> char {
        let n = self.elapsed.as_millis() / FRAME_EVERY.as_millis();
        FRAMES[usize::try_from(n % FRAMES.len() as u128).unwrap_or(0)]
    }

    /// ¿Afecta a `pane`? Falso para el trabajo de sesión: la cabecera de un
    /// panel no debe girar por algo que no le pasa a él.
    #[must_use]
    pub fn affects(&self, pane: usize) -> bool {
        self.pane == Some(pane)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn con(elapsed: Duration) -> Busy {
        Busy {
            elapsed,
            ..Busy::new(BusyKind::Connecting, None, Some(1))
        }
    }

    /// El umbral es la mitad del valor: sin él, cada `cd` local pintaría un
    /// destello y el indicador dejaría de mirarse justo cuando importa.
    #[test]
    fn nada_antes_del_umbral_y_algo_despues() {
        assert!(!con(Duration::ZERO).visible());
        assert!(
            !con(THRESHOLD
                .checked_sub(Duration::from_millis(1))
                .expect("el umbral es mayor que 1 ms"))
            .visible()
        );
        assert!(con(THRESHOLD).visible(), "el umbral es inclusivo");
        assert!(con(Duration::from_secs(3)).visible());
    }

    /// El fotograma sale del TIEMPO, así que dos superficies que pinten a
    /// ritmos distintos enseñan el mismo, y avanza de verdad (no se queda
    /// clavado, que es la mentira que este módulo existe para no contar).
    #[test]
    fn el_spinner_avanza_con_el_tiempo_y_da_la_vuelta() {
        let f = |ms| con(Duration::from_millis(ms)).frame();
        assert_eq!(f(0), FRAMES[0]);
        assert_eq!(f(79), FRAMES[0], "dentro del mismo fotograma no salta");
        assert_eq!(f(80), FRAMES[1]);
        assert_ne!(f(0), f(240), "en tres fotogramas ha cambiado");
        // Da la vuelta sin salirse: la duración no está acotada.
        assert_eq!(f(80 * FRAMES.len() as u64), FRAMES[0]);
        assert_eq!(con(Duration::from_hours(24)).frame(), FRAMES[0]);
    }

    /// Ninguna clase de trabajo puede quedarse sin etiqueta: un spinner sin
    /// verbo dice «espera» y no dice a qué, que es la mitad de la queja.
    #[test]
    fn cada_clase_tiene_su_clave_y_ninguna_se_repite() {
        let claves: Vec<_> = BusyKind::all().iter().map(|k| k.key()).collect();
        for (i, a) in claves.iter().enumerate() {
            assert!(!a.is_empty());
            for b in &claves[i + 1..] {
                assert_ne!(a, b, "dos clases con la misma clave: {a}");
            }
        }
    }

    /// Un trabajo de sesión (sin panel) no hace girar la cabecera de NINGUNO:
    /// marcaría como ocupado un panel al que no le está pasando nada.
    #[test]
    fn el_trabajo_sin_panel_no_marca_ninguno() {
        let global = Busy::new(BusyKind::Listing, None, None);
        assert!(!global.affects(0));
        assert!(!global.affects(1));
        let del_uno = Busy::new(BusyKind::Connecting, None, Some(1));
        assert!(del_uno.affects(1));
        assert!(!del_uno.affects(0));
    }
}
