//! Una línea de log como DATO, sin nada de `tracing`.
//!
//! Está fuera de la feature `logging` a propósito. Quien produce las líneas es
//! el subscriber (y arrastra `tracing-subscriber` y `tracing-appender`), pero
//! quien las PINTA es un frontend, y un frontend de presentación no tiene por
//! qué compilar un subscriber para saber dibujar una lista. Con el tipo aquí,
//! `norte-frontend` filtra y maqueta sin dependencias nuevas, y la ventana
//! gráfica hereda lo mismo (ADR 0077: la decisión de presentación se toma una
//! vez).

/// El nivel de una línea, de menos a más verboso.
///
/// Propio y no `tracing::Level` por lo dicho arriba, y `Ord` porque la
/// pregunta que se le hace siempre es «¿es como mucho tan verboso como lo que
/// se está enseñando?».
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LogLevel {
    /// Algo falló.
    Error,
    /// Algo va a fallar, o se degradó.
    Warn,
    /// Lo que pasa, en condiciones normales.
    Info,
    /// Detalle para depurar.
    Debug,
    /// Todo.
    Trace,
}

impl LogLevel {
    /// La etiqueta de cinco columnas que se pinta, ya alineada.
    ///
    /// Ancho FIJO: una columna de niveles que baila deja el mensaje empezando
    /// en sitios distintos y la lista deja de poder recorrerse con la vista.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warn => "WARN ",
            Self::Info => "INFO ",
            Self::Debug => "DEBUG",
            Self::Trace => "TRACE",
        }
    }

    /// El identificador ESTABLE, para el puente de la ventana (#326).
    ///
    /// Separado de [`Self::label`], que es lo que se PINTA: aquella lleva su
    /// relleno de cinco columnas y podría cambiar de forma el día que la
    /// columna cambie de ancho. Esto es un vocabulario cerrado que un renderer
    /// compara por igualdad para colorear y para marcar cuál está puesto, y
    /// comparar contra una etiqueta de pantalla ataría el color al ancho.
    ///
    /// ```
    /// use norte_config::logline::LogLevel;
    /// assert_eq!(LogLevel::Warn.wire(), "warn");
    /// assert_eq!(LogLevel::from_wire("warn"), Some(LogLevel::Warn));
    /// // Uno que no existe no cae en otro: se dice que no se conoce.
    /// assert_eq!(LogLevel::from_wire("verbose"), None);
    /// ```
    #[must_use]
    pub const fn wire(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::Trace => "trace",
        }
    }

    /// El nivel de un identificador de wire, o `None` si no se conoce.
    ///
    /// `None` y no un valor por defecto: caer en `Info` ante algo que no se
    /// entiende dejaría al panel enseñando otra cosa de la que se pidió, en
    /// silencio.
    #[must_use]
    pub fn from_wire(s: &str) -> Option<Self> {
        Self::all().into_iter().find(|l| l.wire() == s)
    }

    /// Todos, del menos al más verboso.
    #[must_use]
    pub const fn all() -> [Self; 5] {
        [
            Self::Error,
            Self::Warn,
            Self::Info,
            Self::Debug,
            Self::Trace,
        ]
    }
}

/// Una línea ya lista para pintar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogLine {
    /// Milisegundos desde la época (UTC), para darle formato con el mismo
    /// criterio que las columnas de fecha.
    pub epoch_ms: i64,
    /// Nivel del evento.
    pub level: LogLevel,
    /// Módulo que lo emitió (`norte_core::connect`).
    pub target: String,
    /// El mensaje y sus campos, ya aplanados.
    pub message: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El orden es el de VERBOSIDAD, que es la comparación que hace el filtro.
    #[test]
    fn el_orden_va_de_menos_a_mas_verboso() {
        assert!(LogLevel::Error < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Trace);
    }

    /// Las etiquetas miden lo mismo: si no, el mensaje empieza en columnas
    /// distintas y la lista no se puede recorrer con la vista.
    #[test]
    fn las_etiquetas_tienen_el_mismo_ancho() {
        for l in LogLevel::all() {
            assert_eq!(l.label().len(), 5, "{l:?} rompe la columna");
        }
    }

    /// **El vocabulario de niveles es EL MISMO que el del protocolo, en las
    /// dos direcciones** (0.65.0, #328).
    ///
    /// Hay dos copias porque tiene que haberlas: `norte-proto` no puede
    /// depender de este crate —la flecha va al revés— así que
    /// `methods::LOG_LEVELS` repite las cinco cadenas como vocabulario del
    /// wire. Y este test es el único sitio del árbol desde el que se ven las
    /// dos, así que es donde vive la igualdad. Es el mismo patrón que el
    /// vocabulario de hashing, donde `norte-core` guarda una copia congelada
    /// por el formato del journal.
    ///
    /// Se comprueba en LAS DOS direcciones a propósito. Solo «cada nivel
    /// nuestro está en el protocolo» dejaría añadir uno al wire que ningún
    /// frontend sabría pintar; solo la inversa dejaría añadir uno aquí que no
    /// se podría pedir por el cable. Y `from_wire` cierra el viaje de vuelta:
    /// que las cadenas coincidan no sirve de nada si la que llega no se sabe
    /// convertir.
    #[test]
    fn el_vocabulario_de_niveles_es_el_mismo_que_el_del_protocolo() {
        use std::collections::BTreeSet;

        let nuestros: BTreeSet<&str> = LogLevel::all().into_iter().map(LogLevel::wire).collect();
        let del_wire: BTreeSet<&str> = norte_proto::methods::LOG_LEVELS.iter().copied().collect();
        assert_eq!(
            nuestros, del_wire,
            "los dos vocabularios de nivel se han separado: renombrar uno es \
             un cambio de wire (bump + golden), y añadir uno hay que hacerlo \
             en los dos sitios"
        );
        for w in norte_proto::methods::LOG_LEVELS {
            assert_eq!(
                LogLevel::from_wire(w).map(LogLevel::wire),
                Some(*w),
                "`{w}` viaja por el cable y no se sabe convertir de vuelta"
            );
        }
    }
}
