//! Una contraseña a medio teclear, compartida por los dos frontends.
//!
//! Vive aquí y no en un frontend por la regla D14, y esta vez con el motivo
//! más fuerte de todos: es un tipo de SEGURIDAD. Dos implementaciones de «lo
//! tecleado en el campo de una contraseña» son dos sitios donde el `Debug` que
//! redacta se olvida, dos sitios donde el buffer no se pisa al soltarlo, y
//! —peor— un frontend que hereda la versión sin ninguna de las dos cosas
//! porque nació después. La TUI lo tenía desde #325; la ventana lo necesita
//! para #327, y lo que se comparte es la GARANTÍA, no el parecido.

use zeroize::Zeroizing;

/// Tope de caracteres de una contraseña tecleada.
///
/// El mismo que los campos de texto de la TUI (`TEXT_FIELD_MAX_CHARS`), y aquí
/// además es estructural: fija la capacidad que se reserva de antemano, así
/// que pasarse significa reasignar — que es exactamente lo que deja trozos de
/// la contraseña sin pisar en el heap. Ver «Por qué reserva sitio de antemano».
pub const SECRET_MAX_CHARS: usize = 256;

/// Los BYTES que hay que reservar para [`SECRET_MAX_CHARS`] caracteres.
///
/// Cuatro por carácter, que es el máximo de UTF-8. Y no es una holgura de
/// cortesía: la reserva se hacía con el número de CARACTERES, o sea 256 bytes,
/// mientras el tope se aplicaba en caracteres. Una contraseña de 150 letras
/// acentuadas son 300 bytes y menos de 256 caracteres — o sea que cabía en el
/// tope y NO cabía en la reserva, así que `String` reasignaba, copiaba y
/// liberaba el bloque viejo **sin pisarlo**: la mitad de la contraseña quedaba
/// en el heap. Exactamente lo que el tipo promete que no pasa.
const RESERVA_BYTES: usize = SECRET_MAX_CHARS * 4;

/// Lo tecleado en un campo de contraseña.
///
/// Existe por dos cosas que un `String` no da, y ninguna es opcional (regla
/// 10):
///
/// * **`Debug` que REDACTA.** Los modales derivan `Debug`, y ese `Debug` acaba
///   en `tracing`, en el mensaje de un panic y en el diff de un `assert_eq!`.
///   `Zeroizing<String>` delega su `Debug` en el `String`, así que sin este
///   envoltorio la contraseña se imprimiría en los tres sitios.
/// * **Borrado al soltar.** El interior es `Zeroizing`: el buffer se pisa con
///   ceros en el drop, en vez de quedarse en el heap para un core dump o el
///   swap.
///
/// # Por qué reserva sitio de antemano
///
/// `Zeroizing` borra la asignación ACTUAL entera, capacidad incluida — y solo
/// esa: su propia documentación dice que «cannot ensure that previous
/// reallocations did not leave values on the heap». Un `String` que crece
/// 4→8→16→… va dejando por el camino trozos sin pisar de la contraseña a medio
/// escribir. Naciendo con los BYTES de [`SECRET_MAX_CHARS`] caracteres
/// reservados —cuatro por carácter; ver `RESERVA_BYTES` para por qué contar
/// caracteres ahí era un fallo— no hay ninguna reasignación, y el «best
/// effort» de zeroize pasa a ser exacto para esta copia. Las copias de más allá
/// del [`TypedSecret::expose`] (los params, el frame serializado, el `Value`
/// del daemon) siguen sin pisarse: ver el ADR 0015, que dice cuáles sí y cuáles
/// no.
///
/// `PartialEq` está derivado para los tests (comparar dos modales) y compara en
/// tiempo NO constante: no le pases nunca un valor de origen ajeno.
///
/// ```
/// use norte_frontend::secret::TypedSecret;
/// let mut s = TypedSecret::default();
/// assert!(s.is_empty());
/// s.push('h');
/// s.push('i');
/// assert_eq!(s.chars(), 2);
/// // Lo que se pinta son PUNTOS, no el texto.
/// assert_eq!(s.dots(), "••");
/// // Y el `Debug` no lo dice.
/// assert_eq!(format!("{s:?}"), "TypedSecret(***)");
/// ```
#[derive(PartialEq, Eq)]
pub struct TypedSecret(Zeroizing<String>);

impl Clone for TypedSecret {
    /// A mano, y no derivado, para que la copia NAZCA con la reserva.
    ///
    /// `String::clone` asigna capacidad igual a la longitud, así que un clon
    /// derivado empieza justo lleno: el primer carácter que se le añadiera lo
    /// haría reasignar, y ahí es donde se deja un trozo de la contraseña sin
    /// pisar. Hoy nadie escribe en un clon —la TUI clona el modal para leerlo—
    /// pero la trampa estaba armada y cuesta tres líneas desarmarla.
    fn clone(&self) -> Self {
        let mut copia = Self::default();
        copia.0.push_str(&self.0);
        copia
    }
}

impl Default for TypedSecret {
    fn default() -> Self {
        // Ver «Por qué reserva sitio de antemano»: `String::new()` aquí
        // reintroduce las reasignaciones y con ellas los restos en el heap.
        Self(Zeroizing::new(String::with_capacity(RESERVA_BYTES)))
    }
}

impl std::fmt::Debug for TypedSecret {
    /// Nunca el contenido. La longitud tampoco: es información sobre la
    /// contraseña, y para depurar basta saber si hay algo escrito.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_empty() {
            "TypedSecret(vacío)"
        } else {
            "TypedSecret(***)"
        })
    }
}

impl TypedSecret {
    /// El texto en claro, para entregarlo por `connection.provide_secret`.
    ///
    /// Llamarlo es decir «aquí SÍ hace falta el secreto» — no lo uses para
    /// pintar ni para registrar.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }

    /// Cuántos caracteres se han tecleado, para pintar los puntos.
    #[must_use]
    pub fn chars(&self) -> usize {
        self.0.chars().count()
    }

    /// ¿Está vacío? Confirmar sobre un campo vacío no entrega nada.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Lo que se PINTA: un punto por carácter.
    ///
    /// Un método y no una decisión de cada renderer, porque la alternativa es
    /// que uno de los dos pinte el texto. Aquí no hay forma de equivocarse: lo
    /// único que sale de este tipo para la capa de pintado son puntos.
    #[must_use]
    pub fn dots(&self) -> String {
        "•".repeat(self.chars())
    }

    /// Añade un carácter tecleado, hasta [`SECRET_MAX_CHARS`].
    ///
    /// El tope impide que una tecla trabada haga crecer el buffer más allá de
    /// lo reservado — que es cuando `String` reasigna y deja un trozo de la
    /// contraseña sin pisar en el heap. Frenar en mudo es lo que hacen los
    /// otros campos de texto: el campo se ve lleno.
    pub fn push(&mut self, c: char) {
        if self.0.chars().count() >= SECRET_MAX_CHARS {
            return;
        }
        self.0.push(c);
    }

    /// Borra el último carácter (retroceso).
    pub fn pop(&mut self) {
        self.0.pop();
    }

    /// Reemplaza lo tecleado por `texto`, respetando el tope.
    ///
    /// Existe para la ventana, donde el campo lo edita la webview y llega el
    /// texto ENTERO tras cada pulsación en vez de un carácter: el caret es del
    /// renderer, y reconstruirlo en Rust sería mantener dos ideas de dónde
    /// está el cursor. Lo que sobra del tope se descarta sin más — igual que
    /// [`Self::push`] frena en mudo.
    ///
    /// El buffer anterior se pisa con ceros ANTES de escribir el nuevo: sin
    /// eso, teclear seis caracteres dejaría cinco prefijos de la contraseña
    /// intactos en el heap, que es justo lo que este tipo existe para evitar.
    pub fn set(&mut self, texto: &str) {
        use zeroize::Zeroize;
        // `Zeroize for String` pisa los bytes escritos, hace `clear()` y pisa
        // TAMBIÉN la capacidad libre — pero no libera: la asignación
        // sobrevive. Eso es justo lo que hace falta, porque significa que
        // todos los prefijos que se hayan tecleado antes quedan pisados sin
        // reasignar nada. La `reserve` de debajo es defensa en profundidad:
        // sobre un buffer que ya tiene la reserva no hace nada.
        self.0.zeroize();
        self.0.reserve(RESERVA_BYTES);
        for c in texto.chars().take(SECRET_MAX_CHARS) {
            self.0.push(c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// El `Debug` no dice ni el contenido ni la longitud.
    ///
    /// La longitud tampoco es inocente: es información sobre la contraseña, y
    /// este `Debug` acaba en `tracing`, en un panic y en el diff de un
    /// `assert_eq!` sobre el modal entero.
    #[test]
    fn el_debug_redacta() {
        let mut s = TypedSecret::default();
        assert_eq!(format!("{s:?}"), "TypedSecret(vacío)");
        s.set("correcthorsebatterystaple");
        let d = format!("{s:?}");
        assert_eq!(d, "TypedSecret(***)");
        assert!(!d.contains("horse"), "el contenido no sale: {d}");
        assert!(!d.contains("25"), "la longitud tampoco: {d}");
    }

    /// Lo único que sale para pintar son puntos, uno por CARÁCTER.
    ///
    /// Por carácter y no por byte: con bytes, una contraseña con acentos se
    /// pintaría más larga de lo que es, que es filtrar su composición.
    #[test]
    fn solo_salen_puntos_y_uno_por_caracter() {
        let mut s = TypedSecret::default();
        s.set("cañón€");
        assert_eq!(s.chars(), 6);
        assert_eq!(s.dots(), "••••••");
        assert_eq!(s.dots().chars().count(), s.chars());
    }

    /// El tope frena en mudo, y `set` no lo salta.
    ///
    /// Pasarse del tope es reasignar, y reasignar es dejar un trozo de la
    /// contraseña sin pisar en el heap — la razón de ser de la reserva.
    #[test]
    fn el_tope_frena_al_teclear_y_al_reemplazar() {
        let mut s = TypedSecret::default();
        for _ in 0..(SECRET_MAX_CHARS + 50) {
            s.push('x');
        }
        assert_eq!(s.chars(), SECRET_MAX_CHARS);

        let largo = "y".repeat(SECRET_MAX_CHARS + 50);
        s.set(&largo);
        assert_eq!(s.chars(), SECRET_MAX_CHARS);
        assert!(s.expose().chars().all(|c| c == 'y'), "se reemplazó entero");
    }

    /// La reserva se mide en BYTES, no en caracteres, y una contraseña llena
    /// de acentos no reasigna.
    ///
    /// Este era un fallo de verdad: la reserva se hacía con el número de
    /// caracteres (256 bytes) y el tope se aplicaba en caracteres, así que 150
    /// letras acentuadas —300 bytes— cabían en el tope y no en la reserva.
    /// `String` reasignaba, copiaba y liberaba el bloque viejo SIN pisarlo:
    /// media contraseña en el heap, que es exactamente lo que este tipo
    /// promete que no pasa.
    ///
    /// Se comprueba por la CAPACIDAD y no por el puntero porque lo que hay que
    /// afirmar es que nunca hizo falta crecer.
    #[test]
    fn una_contrasena_de_multibyte_no_reasigna() {
        let mut s = TypedSecret::default();
        let cap = s.0.capacity();
        assert!(cap >= SECRET_MAX_CHARS * 4, "la reserva va en bytes: {cap}");
        // El peor caso del tope: 256 caracteres de cuatro bytes cada uno.
        let peor: String = std::iter::repeat_n('\u{1F600}', SECRET_MAX_CHARS).collect();
        s.set(&peor);
        assert_eq!(s.chars(), SECRET_MAX_CHARS);
        assert_eq!(
            s.0.capacity(),
            cap,
            "creció: hubo una reasignación, y el bloque viejo se liberó sin pisar"
        );

        // Y por `push`, que es el camino de la TUI.
        let mut t = TypedSecret::default();
        let cap = t.0.capacity();
        for _ in 0..SECRET_MAX_CHARS {
            t.push('ñ');
        }
        assert_eq!(t.0.capacity(), cap, "el otro camino tampoco reasigna");
    }

    /// Un clon nace CON la reserva, no lleno.
    ///
    /// `String::clone` asigna capacidad igual a la longitud, así que el clon
    /// derivado reasignaba al primer carácter que se le añadiera.
    #[test]
    fn un_clon_nace_con_su_reserva() {
        let mut s = TypedSecret::default();
        s.set("algo");
        let c = s.clone();
        assert_eq!(c.expose(), "algo");
        assert!(
            c.0.capacity() >= SECRET_MAX_CHARS * 4,
            "el clon nació lleno: {}",
            c.0.capacity()
        );
    }

    /// Vaciar y volver a teclear funciona, y `set("")` deja el campo inerte.
    #[test]
    fn vaciar_deja_el_campo_inerte() {
        let mut s = TypedSecret::default();
        s.set("algo");
        assert!(!s.is_empty());
        s.set("");
        assert!(s.is_empty(), "confirmar sobre esto no entrega nada");
        assert_eq!(s.dots(), "");
        s.push('a');
        assert_eq!(s.chars(), 1, "sigue usable tras vaciarlo");
    }

    /// El retroceso quita UN carácter, no un byte.
    #[test]
    fn el_retroceso_quita_un_caracter() {
        let mut s = TypedSecret::default();
        s.set("añ");
        s.pop();
        assert_eq!(s.expose(), "a");
    }
}
