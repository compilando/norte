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
    /// Qué le pasa a esta entrada, si es que le pasa algo (#365).
    ///
    /// `Some` = norte no supo leerla, y entonces la fila **no se puede
    /// elegir**: no hay sitio al que ir. Sigue apareciendo a propósito, porque
    /// el lector escribió esa conexión y esperaba verla — hacerla desaparecer
    /// le dejaría buscando por qué falta, que es justo lo que pasaba cuando una
    /// entrada mala tiraba la lista entera.
    pub problema: Option<String>,
}

impl Row {
    /// Una fila que sí lleva a algún sitio.
    #[must_use]
    pub fn buena(name: String, url: String) -> Self {
        Self {
            name,
            url,
            problema: None,
        }
    }

    /// Una que no, con su motivo. La URL va vacía: no hay ninguna que dar, y
    /// inventarse un texto para el hueco sería pintar algo que nadie escribió.
    #[must_use]
    pub fn inservible(name: String, motivo: String) -> Self {
        Self {
            name,
            url: String::new(),
            problema: Some(motivo),
        }
    }

    /// ¿Se puede ir a ella?
    #[must_use]
    pub fn se_puede_elegir(&self) -> bool {
        self.problema.is_none()
    }
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

    /// La URL de la fila resaltada, si hay alguna Y se puede ir a ella.
    ///
    /// `None` sobre una fila inservible (#365), y eso es lo que impide un
    /// botón muerto: la fila se ve, dice qué le pasa, y confirmarla no navega
    /// a ningún sitio — que es lo honesto, porque no hay sitio.
    #[must_use]
    pub fn chosen(&self) -> Option<&str> {
        self.rows
            .get(self.cursor())
            .filter(|r| r.se_puede_elegir())
            .map(|r| r.url.as_str())
    }

    /// El motivo de la fila resaltada, si es una de las que no valen.
    ///
    /// Lo pinta el frontend al intentar elegirla: enseñar el motivo AHÍ, en el
    /// momento en que el lector lo intenta, es lo que convierte «esto no hace
    /// nada» en «esto no vale, y por esto».
    #[must_use]
    pub fn problema(&self) -> Option<&str> {
        self.rows
            .get(self.cursor())
            .and_then(|r| r.problema.as_deref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filas() -> Vec<Row> {
        vec![
            Row::buena("casa".into(), "sftp://casa/".into()),
            Row::buena("bucket".into(), "s3://bucket/".into()),
        ]
    }

    /// **Una entrada que norte no sabe leer se VE y no se puede elegir**
    /// (#365).
    ///
    /// Las dos mitades importan y ninguna sola basta. Que se vea, porque el
    /// lector la escribió y hacerla desaparecer lo deja buscando por qué
    /// falta — que es lo que pasaba cuando una entrada mala tiraba la lista
    /// entera. Y que no se pueda elegir, porque no hay sitio al que ir:
    /// ofrecerla sería un botón muerto.
    #[test]
    fn una_conexion_inservible_se_ve_pero_no_lleva_a_ningun_sitio() {
        let mut p = ConnectionsPicker::open(vec![
            Row::inservible("rota".into(), "unknown field `password`".into()),
            Row::buena("casa".into(), "sftp://casa/".into()),
        ]);
        assert_eq!(p.rows().len(), 2, "la rota sigue en la lista");
        assert_eq!(p.chosen(), None, "y no se puede ir a ella");
        assert_eq!(
            p.problema(),
            Some("unknown field `password`"),
            "y dice qué le pasa, que es lo accionable"
        );
        // La buena de al lado sigue siendo elegible: ése es el arreglo.
        p.down();
        assert_eq!(p.chosen(), Some("sftp://casa/"));
        assert_eq!(p.problema(), None);
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
