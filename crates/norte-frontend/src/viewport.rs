//! La ventana visible de una lista larga, y la única regla que la mueve.
//!
//! Vive aparte porque la comparten TRES listas —el listado de ficheros, el
//! panel de diferencias y los pasos de un plan de sincronización— y las tres
//! tenían el mismo defecto: el desplazamiento se deducía del cursor
//! (`selected - (alto-1)`), así que pasada la primera pantalla el cursor
//! quedaba clavado en la última fila y el contenido se movía en cada
//! pulsación. Una regla escrita tres veces es una regla que se arregla una vez
//! y sigue mal en las otras dos.

/// La ventana que hay que pintar: la anterior, arrastrada lo justo para que
/// `cursor` quepa.
///
/// Pura y probada aparte porque es la regla entera: el cursor se mueve DENTRO
/// de la ventana, y solo cuando se sale la ventana le sigue, una fila por
/// fila. Los dos clamps de después importan tanto como eso — una ventana que
/// sobrevive a un listado más corto enseñaría blanco debajo de filas que
/// existen.
#[must_use]
pub fn sticky_offset(previo: usize, cursor: usize, total: usize, rows: usize) -> usize {
    if rows == 0 || total == 0 {
        return 0;
    }
    // Nunca más allá de lo que hay: al encoger el listado (o crecer la
    // terminal) la ventana se re-encuadra sin tocar el cursor.
    let tope = total.saturating_sub(rows);
    let mut off = previo.min(tope);
    let cursor = cursor.min(total - 1);
    if cursor < off {
        // Se salió por arriba: la ventana empieza en él.
        off = cursor;
    } else if cursor >= off + rows {
        // Por abajo: él queda en la ÚLTIMA fila, que es lo que hace que bajar
        // desde el borde mueva exactamente una fila.
        off = cursor + 1 - rows;
    }
    off
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #108 L7: `set_sort` re-ordena en sitio, re-ancla el cursor por PATH
    /// y no toca las marcas (van por identidad); `extend` bajo el spec
    /// activo mergea en el orden nuevo.
    /// La ventana pegajosa, que es la regla entera del scroll del listado.
    ///
    /// Lo que se rompió y por qué se nota: el offset se deducía del cursor
    /// (`selected - (alto-1)`), o sea que pasada la primera pantalla el cursor
    /// vivía CLAVADO en la última fila y cada pulsación movía el contenido.
    /// Al volver hacia arriba la lista bajaba con él y el cursor no se
    /// despegaba nunca del borde — que es exactamente lo que se siente raro.
    #[test]
    fn la_ventana_solo_se_mueve_cuando_el_cursor_toca_un_borde() {
        // Diez filas de ventana sobre cien.
        // Bajar DENTRO no la mueve.
        assert_eq!(sticky_offset(0, 5, 100, 10), 0);
        assert_eq!(sticky_offset(0, 9, 100, 10), 0, "la última fila visible");
        // Tocar el borde inferior la mueve UNA fila.
        assert_eq!(sticky_offset(0, 10, 100, 10), 1);
        // Y subir dentro de la ventana tampoco la mueve: el cursor sube solo.
        assert_eq!(sticky_offset(20, 25, 100, 10), 20);
        assert_eq!(sticky_offset(20, 20, 100, 10), 20, "la primera visible");
        // Hasta tocar el borde superior.
        assert_eq!(sticky_offset(20, 19, 100, 10), 19);
        // Un salto largo (Home/End, un hit de búsqueda) reencuadra de golpe.
        assert_eq!(sticky_offset(20, 0, 100, 10), 0);
        assert_eq!(sticky_offset(20, 99, 100, 10), 90);
    }

    /// Y no sobrevive a un listado que encoge ni a una terminal que crece: una
    /// ventana más allá del final pinta blanco debajo de filas que existen.
    #[test]
    fn la_ventana_se_reencuadra_sin_mover_el_cursor() {
        // El listado pasa de 100 a 12 filas con la ventana en 90.
        assert_eq!(sticky_offset(90, 5, 12, 10), 2, "tope = total - alto");
        // La terminal crece: cabe todo y no hay nada que desplazar.
        assert_eq!(sticky_offset(90, 5, 12, 20), 0);
        // Casos límite: sin filas o sin ventana, no hay desplazamiento.
        assert_eq!(sticky_offset(7, 3, 0, 10), 0);
        assert_eq!(sticky_offset(7, 3, 100, 0), 0);
    }
}
