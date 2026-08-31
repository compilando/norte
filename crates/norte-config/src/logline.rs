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
}
