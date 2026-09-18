//! El selector de conexiones (#140): qué conexiones hay configuradas y cuál
//! se está eligiendo.
//!
//! Vive aquí y no en un frontend por la regla 7, y porque la GUI necesitará
//! el mismo selector con otro pintor. Lo que este módulo NO hace es leer el
//! fichero: se le pasan las filas ya leídas, igual que al selector de
//! disposiciones — quien tiene el disco delante es el frontend.

/// Una conexión configurada.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// El nombre con el que está en `connections.toml`.
    pub name: String,
    /// Su URL. **Nunca un secreto**: un `ConnectionSpec` referencia sus
    /// credenciales (ADR 0015) y aquí solo viaja la dirección.
    pub url: String,
}

/// El selector de conexiones.
#[derive(Debug)]
pub struct ConnectionsPicker {
    rows: Vec<Row>,
    cursor: usize,
}

impl ConnectionsPicker {
    /// Abre el selector con las conexiones que se le pasen.
    ///
    /// Una lista VACÍA es legítima —no tener conexiones configuradas es lo
    /// normal el primer día— y se puede abrir igual: el frontend pinta que no
    /// hay ninguna y dónde se ponen, que es más útil que una tecla muda.
    #[must_use]
    pub fn open(rows: Vec<Row>) -> Self {
        Self { rows, cursor: 0 }
    }

    /// Las filas, en el orden en que llegaron.
    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Dónde está el cursor, acotado a las filas que hay.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.rows.len().saturating_sub(1))
    }

    /// Sube.
    pub const fn up(&mut self) {
        self.cursor = self.cursor.saturating_sub(1);
    }

    /// Baja.
    pub fn down(&mut self) {
        self.cursor = (self.cursor + 1).min(self.rows.len().saturating_sub(1));
    }

    /// La URL de la fila resaltada, si hay alguna.
    #[must_use]
    pub fn chosen(&self) -> Option<&str> {
        self.rows.get(self.cursor()).map(|r| r.url.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filas() -> Vec<Row> {
        vec![
            Row {
                name: "casa".into(),
                url: "sftp://casa/".into(),
            },
            Row {
                name: "bucket".into(),
                url: "s3://bucket/".into(),
            },
        ]
    }

    /// El cursor se mueve y se queda DENTRO por los dos extremos: un selector
    /// que se sale por arriba elige lo que no se está mirando.
    #[test]
    fn el_cursor_no_se_sale_por_ningun_extremo() {
        let mut p = ConnectionsPicker::open(filas());
        p.up();
        assert_eq!(p.cursor(), 0);
        p.down();
        p.down();
        p.down();
        assert_eq!(p.cursor(), 1);
        assert_eq!(p.chosen(), Some("s3://bucket/"));
    }

    /// Sin conexiones configuradas el selector se abre igual y no elige nada:
    /// enseñar «no tienes ninguna» es más útil que una tecla que no hace nada.
    #[test]
    fn sin_conexiones_se_abre_y_no_elige_nada() {
        let mut p = ConnectionsPicker::open(Vec::new());
        assert!(p.rows().is_empty());
        assert_eq!(p.chosen(), None);
        p.down();
        assert_eq!(p.cursor(), 0);
    }
}
