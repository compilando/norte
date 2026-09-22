//! El formulario de una búsqueda: qué se pregunta y cómo se convierte en
//! [`FsSearchParams`].
//!
//! Vive aquí, y no en un frontend, por lo mismo que
//! [`crate::search_status`] (ADR 0077): los dos
//! frontends preguntan la misma búsqueda, y el día que cada uno construya sus
//! propios parámetros divergen en silencio. Un filtro que un frontend aplica y
//! el otro no, no se ve como un fallo: se ve como una búsqueda que encontró
//! más cosas.
//!
//! **El reloj se DICE, no se lee.** «Cambiado hace siete días» se cuenta desde
//! el instante en que se pulsa Enter, y quien llama lo sabe; un mapeo que
//! preguntara la hora por su cuenta no se podría probar sin esperar.
//!
//! Aquí no se pinta nada: las etiquetas son CLAVES Fluent
//! ([`SearchField::clave`]) y cada frontend las traduce y las coloca.

use norte_proto::VPath;
use norte_proto::methods::FsSearchParams;

/// Qué campo del formulario recibe lo que se teclea.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchField {
    /// Patrón sobre el NOMBRE (glob o regex).
    Name,
    /// Texto/regex sobre el CONTENIDO.
    Content,
    /// Nombres de carpeta que no se bajan (protocolo 0.81.0).
    Exclude,
    /// Tamaño mínimo.
    MinSize,
    /// Tamaño máximo.
    MaxSize,
    /// Modificado en los últimos N días.
    Days,
    /// Codificación forzada del contenido.
    Encoding,
}

impl SearchField {
    /// Todos, en el orden en que se recorren y en que se pintan.
    ///
    /// Una sola lista para las dos cosas, y eso es deliberado: dos listas
    /// escritas a mano se separan en cuanto entra un campo, y entonces el
    /// cursor salta a una línea que no está pintada.
    pub const ORDEN: [SearchField; 7] = [
        SearchField::Name,
        SearchField::Content,
        SearchField::Exclude,
        SearchField::MinSize,
        SearchField::MaxSize,
        SearchField::Days,
        SearchField::Encoding,
    ];

    /// La clave Fluent de su etiqueta.
    #[must_use]
    pub fn clave(self) -> &'static str {
        match self {
            Self::Name => "search-name",
            Self::Content => "search-content",
            Self::Exclude => "search-exclude",
            Self::MinSize => "search-min-size",
            Self::MaxSize => "search-max-size",
            Self::Days => "search-days",
            Self::Encoding => "search-encoding",
        }
    }

    /// Un id ESTABLE para el cable y para los tests.
    ///
    /// No es la clave Fluent —esa puede cambiar de nombre con la redacción— ni
    /// el índice, que se renumera al insertar un campo. Es lo que viaja en
    /// `UiAction::DialogField` y lo que un test nombra.
    #[must_use]
    pub fn id(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Content => "content",
            Self::Exclude => "exclude",
            Self::MinSize => "min-size",
            Self::MaxSize => "max-size",
            Self::Days => "days",
            Self::Encoding => "encoding",
        }
    }

    /// El campo con ese id, si alguno. Un id desconocido es `None`: lo manda
    /// un renderer, y un renderer no decide qué campos hay.
    #[must_use]
    pub fn por_id(id: &str) -> Option<Self> {
        Self::ORDEN.into_iter().find(|f| f.id() == id)
    }
}

/// El id ESTABLE del interruptor de regex, para el cable y los tests.
///
/// Los cinco viven aquí, con el modelo, y no en el frontend que los pinta: un
/// id es parte de lo que el formulario ES —lo que vuelve cuando alguien toca
/// un control— y tenerlos en el renderer los dejaría a merced de quien
/// reordene la pantalla.
pub const ID_REGEX: &str = "regex";
/// El id estable del interruptor de mayúsculas. Ver [`ID_REGEX`].
pub const ID_CASE: &str = "case";
/// El id estable del interruptor de palabra entera. Ver [`ID_REGEX`].
pub const ID_WHOLE_WORD: &str = "whole-word";
/// El id estable del interruptor de subcarpetas. Ver [`ID_REGEX`].
pub const ID_RECURSIVE: &str = "recursive";
/// El id estable del ciclo de clases de entrada. Ver [`ID_REGEX`].
pub const ID_KINDS: &str = "kinds";

/// Qué clase de entrada cuenta como resultado de una búsqueda.
///
/// Tres valores y no una lista libre: son las tres respuestas que la gente da,
/// y un selector de las siete clases de `S_IFMT` para encontrar un socket es un
/// diálogo más grande al servicio de nadie.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchKinds {
    /// Lo que sea.
    #[default]
    Todo,
    /// Solo ficheros.
    Ficheros,
    /// Solo carpetas.
    Carpetas,
}

impl SearchKinds {
    /// El siguiente del ciclo.
    #[must_use]
    pub fn siguiente(self) -> Self {
        match self {
            Self::Todo => Self::Ficheros,
            Self::Ficheros => Self::Carpetas,
            Self::Carpetas => Self::Todo,
        }
    }

    /// La clave Fluent de su etiqueta.
    #[must_use]
    pub fn clave(self) -> &'static str {
        match self {
            Self::Todo => "search-kinds-any",
            Self::Ficheros => "search-kinds-files",
            Self::Carpetas => "search-kinds-dirs",
        }
    }

    /// Lo que va en `FsSearchParams::kinds`. Vacío = todas.
    #[must_use]
    pub fn wire(self) -> Vec<norte_proto::EntryKind> {
        match self {
            Self::Todo => Vec::new(),
            Self::Ficheros => vec![norte_proto::EntryKind::File],
            Self::Carpetas => vec![norte_proto::EntryKind::Dir],
        }
    }
}

/// Un tamaño escrito a mano: `1024`, `500k`, `1M`, `2.5G`, `  3 g  `.
///
/// `None` si no se entiende, y eso incluye la cadena vacía: quien llama
/// distingue «no puso nada» de «puso algo ilegible» mirando si el campo está
/// en blanco. Las unidades son potencias de 1024, que es lo que enseña la
/// columna de tamaño; un `k` minúscula y una `K` mayúscula son lo mismo,
/// porque teclear la caja correcta de una unidad no es una decisión.
///
/// ```
/// use norte_frontend::search::parse_size;
/// assert_eq!(parse_size("1k"), Some(1024));
/// assert_eq!(parse_size("2.5G"), Some(2_684_354_560));
/// assert_eq!(parse_size("1 giga"), None);
/// ```
#[must_use]
pub fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (numero, mult) = match s.chars().last()?.to_ascii_lowercase() {
        'k' => (&s[..s.len() - 1], 1024_u64),
        'm' => (&s[..s.len() - 1], 1024 * 1024),
        'g' => (&s[..s.len() - 1], 1024 * 1024 * 1024),
        't' => (&s[..s.len() - 1], 1024_u64.pow(4)),
        _ => (s, 1),
    };
    let n: f64 = numero.trim().parse().ok()?;
    if !n.is_finite() || n < 0.0 {
        return None;
    }
    // `2.5M` es legítimo y `2.5` bytes no, así que se redondea al entero más
    // cercano DESPUÉS de multiplicar.
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        clippy::cast_precision_loss,
        reason = "acotado justo arriba: finito, no negativo y comparado contra u64::MAX"
    )]
    {
        let bytes = n * mult as f64;
        (bytes <= u64::MAX as f64).then_some(bytes.round() as u64)
    }
}

/// Un número de días: entero, no negativo y con un tope de cien años.
///
/// El tope no protege de nada aritmético —restar cien años de 2026 da 1926,
/// que es un `mtime_ms` negativo perfectamente legal y que el filtro del core
/// compara igual—, sino de un dedo: `20260920` en el campo de días es una
/// fecha mal puesta, y aceptarla como «hace cincuenta y cinco mil años» es lo
/// mismo que no filtrar.
///
/// ```
/// use norte_frontend::search::parse_days;
/// assert_eq!(parse_days("7"), Some(7));
/// assert_eq!(parse_days("36501"), None);
/// ```
#[must_use]
pub fn parse_days(s: &str) -> Option<u32> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    s.parse::<u32>().ok().filter(|d| *d <= 36_500)
}

/// Lo que se pregunta en una búsqueda: siete campos de texto, cuatro
/// interruptores y qué clase de entrada cuenta.
///
/// Es el modelo, no la pantalla: qué tecla mueve cada cosa y cómo se pinta lo
/// decide cada frontend.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "son los INTERRUPTORES del formulario: regex, mayúsculas, \
              palabra entera y subcarpetas. Agruparlos en un tipo aparte no \
              dice nada que sus nombres no digan ya"
)]
pub struct SearchForm {
    /// Patrón de nombre (glob o, con `regex`, regex).
    pub name: String,
    /// Texto de contenido (literal o, con `regex`, regex).
    pub content: String,
    /// Nombres de carpeta que NO se bajan, separados por comas: `target,
    /// node_modules, .git` (protocolo 0.81.0). Globs, como el nombre.
    pub exclude: String,
    /// Tamaño mínimo, en humano: `1M`, `500k`, `1024`. Vacío = sin mínimo.
    pub min_size: String,
    /// Tamaño máximo, mismo formato.
    pub max_size: String,
    /// Modificado en los últimos N DÍAS. Vacío = cualquier fecha.
    ///
    /// Días y no un rango de fechas porque es la pregunta que se hace de
    /// verdad —«¿qué he tocado esta semana?»— y porque un rango pide dos
    /// campos, un formato y una zona horaria para contestar lo mismo.
    pub days: String,
    /// La codificación con la que leer el contenido. Vacío = automática.
    pub encoding: String,
    /// Campo que recibe lo que se teclea.
    pub field: SearchField,
    /// Interpreta ambos patrones como regex en vez de glob/literal.
    pub regex: bool,
    /// Matching sensible a mayúsculas.
    pub case: bool,
    /// La coincidencia de contenido es una palabra entera.
    pub whole_word: bool,
    /// Recorrer los subdirectorios. Encendido de serie.
    pub recursive: bool,
    /// Qué clase de entrada cuenta como resultado.
    pub kinds: SearchKinds,
}

impl Default for SearchForm {
    fn default() -> Self {
        Self {
            name: String::new(),
            content: String::new(),
            exclude: String::new(),
            min_size: String::new(),
            max_size: String::new(),
            days: String::new(),
            encoding: String::new(),
            field: SearchField::Name,
            regex: false,
            case: false,
            whole_word: false,
            recursive: true,
            kinds: SearchKinds::Todo,
        }
    }
}

impl SearchForm {
    /// Formulario vacío con el foco en el campo de nombre.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// El campo de texto activo, mutable.
    fn active_mut(&mut self) -> &mut String {
        self.campo_mut(self.field)
    }

    /// Un campo de texto cualquiera, mutable.
    fn campo_mut(&mut self, f: SearchField) -> &mut String {
        match f {
            SearchField::Name => &mut self.name,
            SearchField::Content => &mut self.content,
            SearchField::Exclude => &mut self.exclude,
            SearchField::MinSize => &mut self.min_size,
            SearchField::MaxSize => &mut self.max_size,
            SearchField::Days => &mut self.days,
            SearchField::Encoding => &mut self.encoding,
        }
    }

    /// El texto de un campo, para pintarlo.
    #[must_use]
    pub fn texto(&self, f: SearchField) -> &str {
        match f {
            SearchField::Name => &self.name,
            SearchField::Content => &self.content,
            SearchField::Exclude => &self.exclude,
            SearchField::MinSize => &self.min_size,
            SearchField::MaxSize => &self.max_size,
            SearchField::Days => &self.days,
            SearchField::Encoding => &self.encoding,
        }
    }

    /// Fija el texto ENTERO de un campo.
    ///
    /// Es lo que necesita un frontend cuyo campo de texto lo edita el propio
    /// toolkit: allí el dueño del caret es el `<input>`, y lo que llega es el
    /// texto resultante, no la tecla. El terminal usa
    /// [`Self::push_char`]/[`Self::backspace`], que es lo que allí ocurre.
    pub fn set_texto(&mut self, f: SearchField, texto: String) {
        *self.campo_mut(f) = texto;
    }

    /// Un carácter imprimible al campo activo.
    pub fn push_char(&mut self, c: char) {
        self.active_mut().push(c);
    }

    /// Backspace en el campo activo.
    pub fn backspace(&mut self) {
        self.active_mut().pop();
    }

    /// Al siguiente campo, en círculo.
    pub fn toggle_field(&mut self) {
        let i = SearchField::ORDEN
            .iter()
            .position(|f| *f == self.field)
            .unwrap_or(0);
        self.field = SearchField::ORDEN[(i + 1) % SearchField::ORDEN.len()];
    }

    /// Alterna glob/literal ⇄ regex (aplica a AMBOS ejes).
    pub fn toggle_regex(&mut self) {
        self.regex = !self.regex;
    }

    /// Alterna la sensibilidad a mayúsculas.
    pub fn toggle_case(&mut self) {
        self.case = !self.case;
    }

    /// Alterna «palabra entera» en la búsqueda de contenido.
    pub fn toggle_whole_word(&mut self) {
        self.whole_word = !self.whole_word;
    }

    /// Alterna el recorrido de subdirectorios.
    pub fn toggle_recursive(&mut self) {
        self.recursive = !self.recursive;
    }

    /// Cicla qué clase de entrada cuenta.
    pub fn cycle_kinds(&mut self) {
        self.kinds = self.kinds.siguiente();
    }

    /// ¿Hay algún criterio? Sin ninguno no se lanza: una búsqueda sin criterio
    /// es un listado recursivo con otro nombre.
    ///
    /// Un FILTRO cuenta como criterio desde 0.81.0: «todo lo que pese más de
    /// un giga» es una búsqueda legítima y de las más útiles que hay. Lo que
    /// no cuenta es excluir carpetas —eso quita, no pide— ni la codificación,
    /// que dice CÓMO leer algo que nadie ha pedido aún.
    #[must_use]
    pub fn has_criteria(&self) -> bool {
        !self.name.is_empty()
            || !self.content.is_empty()
            || self.kinds != SearchKinds::Todo
            || parse_size(&self.min_size).is_some()
            || parse_size(&self.max_size).is_some()
            || parse_days(&self.days).is_some()
    }

    /// Los nombres de carpeta a excluir, uno por coma y sin los vacíos.
    #[must_use]
    pub fn exclude_names(&self) -> Vec<String> {
        self.exclude
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// El campo que se escribió y no se puede entender, si alguno.
    ///
    /// Se comprueba ANTES de lanzar: una búsqueda que ignora en silencio un
    /// `1 gigabyte` mal escrito devuelve el árbol entero y se lee igual que un
    /// resultado, que es exactamente lo que el aviso de versión de 0.81.0
    /// existe para evitar contra un daemon viejo. La misma trampa dentro de
    /// casa no es mejor.
    #[must_use]
    pub fn campo_ilegible(&self) -> Option<SearchField> {
        if !self.min_size.trim().is_empty() && parse_size(&self.min_size).is_none() {
            return Some(SearchField::MinSize);
        }
        if !self.max_size.trim().is_empty() && parse_size(&self.max_size).is_none() {
            return Some(SearchField::MaxSize);
        }
        if !self.days.trim().is_empty() && parse_days(&self.days).is_none() {
            return Some(SearchField::Days);
        }
        // El tope de exclusiones es del PROTOCOLO
        // ([`norte_proto::methods::SEARCH_EXCLUDES_MAX`]): pasarse no es un
        // error del daemon que se pueda leer, es un `InvalidParams` genérico
        // que llega DESPUÉS de lanzar y que el lector no puede mapear a un
        // campo. Dicho aquí, se señala el campo y no se lanza — que es la
        // regla de todos los demás.
        if self.exclude_names().len() > norte_proto::methods::SEARCH_EXCLUDES_MAX {
            return Some(SearchField::Exclude);
        }
        // La codificación también, y es la que más lo necesita: las tres de
        // arriba degradan callando y ésta tumba la búsqueda entera con un
        // error de la petición, que el frontend pinta con la categoría de
        // `InvalidPath` — o sea «ruta inválida» para un nombre de codificación
        // mal escrito. Dicho aquí, se señala el campo.
        let enc = self.encoding.trim();
        if !enc.is_empty()
            && norte_encoding::Encoding::for_label_no_replacement(enc.as_bytes()).is_none()
        {
            return Some(SearchField::Encoding);
        }
        None
    }
}

/// Los [`FsSearchParams`] de un formulario, con el reloj DICHO.
///
/// El toggle `regex` decide, por eje, `name_glob` vs `name_regex` y `content`
/// vs `content_regex`; un campo vacío no aporta criterio.
///
/// `ahora_ms` es el instante desde el que se cuentan los días, y lo dice quien
/// llama: «cambiado hace siete» se cuenta desde que se pulsa Enter, y el core
/// no tiene por qué saber en qué momento se hizo la pregunta. Que sea un
/// parámetro y no una lectura del reloj es lo que hace esto probable.
///
/// ```
/// use norte_frontend::search::{SearchForm, params};
/// use norte_proto::VPath;
///
/// let mut f = SearchForm::new();
/// f.name = "*.rs".to_owned();
/// f.days = "7".to_owned();
/// let p = params(&f, VPath::parse("mem:///casa").unwrap(), 7 * 86_400_000, 10_000);
/// assert_eq!(p.name_glob.as_deref(), Some("*.rs"));
/// assert_eq!(p.name_regex, None);
/// // Siete días contados desde el instante que se dijo, no desde el reloj.
/// assert_eq!(p.mtime_after, Some(0));
/// ```
#[must_use]
pub fn params(form: &SearchForm, root: VPath, ahora_ms: i64, max_hits: u32) -> FsSearchParams {
    let (name_glob, name_regex) = match (form.name.is_empty(), form.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(form.name.clone()), None),
        (false, true) => (None, Some(form.name.clone())),
    };
    let (content, content_regex) = match (form.content.is_empty(), form.regex) {
        (true, _) => (None, None),
        (false, false) => (Some(form.content.clone()), None),
        (false, true) => (None, Some(form.content.clone())),
    };
    let mtime_after = parse_days(&form.days)
        .map(|d| ahora_ms.saturating_sub(i64::from(d).saturating_mul(86_400_000)));
    let encoding = {
        let e = form.encoding.trim();
        (!e.is_empty()).then(|| e.to_owned())
    };
    FsSearchParams {
        name_glob,
        name_regex,
        content,
        content_regex,
        case_sensitive: form.case,
        max_hits: Some(max_hits),
        kinds: form.kinds.wire(),
        min_size: parse_size(&form.min_size),
        max_size: parse_size(&form.max_size),
        mtime_after,
        mtime_before: None,
        exclude_roots: Vec::new(),
        exclude_names: form.exclude_names(),
        whole_word: form.whole_word,
        recursive: form.recursive,
        encoding,
        ..FsSearchParams::new(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raiz() -> VPath {
        VPath::parse("mem:///casa").expect("wire de test")
    }

    /// Los tamaños que la gente escribe de verdad.
    #[test]
    fn parse_size_entiende_lo_que_se_teclea() {
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("1k"), Some(1024));
        assert_eq!(
            parse_size("1K"),
            Some(1024),
            "la caja de la unidad da igual"
        );
        assert_eq!(parse_size(" 1 M "), Some(1024 * 1024), "y los espacios");
        assert_eq!(parse_size("2.5G"), Some(2_684_354_560), "y los decimales");
        assert_eq!(parse_size("1T"), Some(1024_u64.pow(4)));
    }

    /// Y lo que NO se entiende se dice que no se entiende, en vez de valer
    /// cero y devolver el árbol entero.
    #[test]
    fn parse_size_no_adivina() {
        for malo in ["", "  ", "mucho", "1 giga", "-5", "1kk", "k", "inf", "NaN"] {
            assert_eq!(parse_size(malo), None, "{malo:?}");
        }
    }

    #[test]
    fn parse_days_es_un_entero_acotado() {
        assert_eq!(parse_days("7"), Some(7));
        assert_eq!(parse_days(" 0 "), Some(0));
        assert_eq!(parse_days("36500"), Some(36_500));
        for malo in ["", "-1", "1.5", "36501", "ayer"] {
            assert_eq!(parse_days(malo), None, "{malo:?}");
        }
    }

    /// Un campo ilegible se nombra ANTES de lanzar, y dice cuál.
    #[test]
    fn un_campo_ilegible_se_nombra_y_no_se_lanza() {
        let mut d = SearchForm::new();
        d.min_size = "mucho".into();
        assert_eq!(d.campo_ilegible(), Some(SearchField::MinSize));
        d.min_size = "1M".into();
        assert_eq!(d.campo_ilegible(), None, "ya se entiende");
        d.days = "ayer".into();
        assert_eq!(d.campo_ilegible(), Some(SearchField::Days));
        // Vacío no es ilegible: es «no puse nada».
        d.days = "   ".into();
        assert_eq!(d.campo_ilegible(), None);
        // Y la codificación, que es la que más lo necesita: es la única cuyo
        // error tumba la búsqueda entera con un `InvalidPath` que el frontend
        // pinta como «ruta inválida».
        d.encoding = "utf-ocho".into();
        assert_eq!(d.campo_ilegible(), Some(SearchField::Encoding));
        // Incluida una etiqueta de REEMPLAZO, que no falla pero no encuentra
        // nada: decodifica el fichero entero a un solo U+FFFD.
        d.encoding = "utf-7".into();
        assert_eq!(d.campo_ilegible(), Some(SearchField::Encoding));
        d.encoding = "windows-1252".into();
        assert_eq!(d.campo_ilegible(), None);
    }

    /// Un FILTRO solo ya es un criterio (0.81.0); excluir carpetas no, porque
    /// quita en vez de pedir.
    #[test]
    fn un_filtro_solo_basta_para_lanzar() {
        let mut d = SearchForm::new();
        assert!(!d.has_criteria(), "vacío del todo no");
        d.exclude = "target".into();
        assert!(!d.has_criteria(), "excluir no es pedir");
        d.encoding = "utf-8".into();
        assert!(!d.has_criteria(), "cómo leer algo no es qué buscar");
        d.min_size = "1G".into();
        assert!(d.has_criteria(), "«todo lo que pese más de un giga» sí");
    }

    /// **Los días se cuentan desde el instante que se DICE.**
    ///
    /// Es lo que el mapeo del terminal no podía probar: leía
    /// `SystemTime::now()` por su cuenta, así que el esperado de un test
    /// tenía que calcularse con el mismo reloj que el código bajo prueba.
    #[test]
    fn los_dias_se_cuentan_desde_el_instante_dicho() {
        let mut f = SearchForm::new();
        f.days = "7".into();
        let ahora = 10 * 86_400_000_i64;
        let p = params(&f, raiz(), ahora, 10_000);
        assert_eq!(p.mtime_after, Some(3 * 86_400_000));
        assert_eq!(
            p.mtime_before, None,
            "el formulario no pregunta el otro lado"
        );
        // Sin días no hay filtro de fecha, y no «desde el principio de los
        // tiempos»: un filtro que no se pidió no se manda.
        f.days = String::new();
        assert_eq!(params(&f, raiz(), ahora, 10_000).mtime_after, None);
    }

    /// El interruptor `regex` cambia de eje los DOS patrones a la vez, que es
    /// lo que dice su etiqueta.
    #[test]
    fn el_toggle_de_regex_mueve_los_dos_ejes() {
        let mut f = SearchForm::new();
        f.name = "*.rs".into();
        f.content = "TODO".into();
        let p = params(&f, raiz(), 0, 10_000);
        assert_eq!(p.name_glob.as_deref(), Some("*.rs"));
        assert_eq!(p.content.as_deref(), Some("TODO"));
        assert!(p.name_regex.is_none() && p.content_regex.is_none());

        f.toggle_regex();
        let p = params(&f, raiz(), 0, 10_000);
        assert_eq!(p.name_regex.as_deref(), Some("*.rs"));
        assert_eq!(p.content_regex.as_deref(), Some("TODO"));
        assert!(p.name_glob.is_none() && p.content.is_none());
    }

    /// Lo que el formulario NO pregunta no viaja: un `FsSearchParams` recién
    /// hecho tiene que seguir siendo el de una búsqueda corriente.
    #[test]
    fn un_formulario_vacio_no_manda_filtros() {
        let p = params(&SearchForm::new(), raiz(), 0, 10_000);
        assert!(p.kinds.is_empty());
        assert!(p.exclude_names.is_empty());
        assert!(p.min_size.is_none() && p.max_size.is_none());
        assert!(p.encoding.is_none());
        assert!(!p.whole_word);
        assert!(p.recursive, "recorrer subdirectorios es lo que hacía");
    }

    /// Las exclusiones se separan por comas y los huecos no cuentan.
    #[test]
    fn las_exclusiones_se_separan_por_comas() {
        let mut f = SearchForm::new();
        f.exclude = " target , node_modules ,, .git ".into();
        assert_eq!(f.exclude_names(), ["target", "node_modules", ".git"]);
    }

    /// **Pasarse del tope de exclusiones se dice ANTES de lanzar.**
    ///
    /// El tope es del protocolo, y el daemon lo hace cumplir con un
    /// `InvalidParams` genérico que llega después de lanzar: el lector ve una
    /// búsqueda que falló y no puede saber por qué campo. Comprobado aquí, se
    /// señala el campo — y los dos frontends lo heredan, que es para lo que
    /// existe este módulo.
    #[test]
    fn pasarse_del_tope_de_exclusiones_senala_el_campo() {
        let tope = norte_proto::methods::SEARCH_EXCLUDES_MAX;
        let mut f = SearchForm::new();
        f.name = "*.rs".into();
        f.exclude = vec!["a"; tope].join(",");
        assert_eq!(f.campo_ilegible(), None, "justo en el tope se admite");
        f.exclude = vec!["a"; tope + 1].join(",");
        assert_eq!(f.campo_ilegible(), Some(SearchField::Exclude));
    }

    /// El id de un campo es ESTABLE y va y vuelve; uno que no existe no se
    /// inventa (lo manda un renderer).
    #[test]
    fn los_ids_de_campo_van_y_vuelven() {
        for f in SearchField::ORDEN {
            assert_eq!(SearchField::por_id(f.id()), Some(f), "{:?}", f.id());
        }
        assert_eq!(SearchField::por_id("no-existe"), None);
    }

    /// **Los ids de los interruptores no pueden chocar con los de los campos.**
    ///
    /// El host resuelve un id preguntando primero por [`SearchField::por_id`] y
    /// cayendo a los cinco `ID_*` si no es ninguno. El día que un campo se
    /// llamara `case`, el interruptor dejaría de funcionar sin que nada se
    /// pusiera rojo: esto es lo que se pone rojo.
    #[test]
    fn los_ids_de_interruptor_no_chocan_con_los_de_campo() {
        for id in [ID_REGEX, ID_CASE, ID_WHOLE_WORD, ID_RECURSIVE, ID_KINDS] {
            assert_eq!(SearchField::por_id(id), None, "{id} choca con un campo");
        }
    }

    /// El foco recorre los siete en círculo, en el orden en que se pintan.
    #[test]
    fn el_foco_recorre_los_siete_en_circulo() {
        let mut f = SearchForm::new();
        assert_eq!(f.field, SearchField::Name);
        for esperado in SearchField::ORDEN.into_iter().skip(1) {
            f.toggle_field();
            assert_eq!(f.field, esperado);
        }
        f.toggle_field();
        assert_eq!(f.field, SearchField::Name, "y vuelve al primero");
    }

    /// Escribir un campo entero es lo que hace la ventana; teclear, lo que
    /// hace el terminal. Los dos acaban en el mismo sitio.
    #[test]
    fn se_puede_escribir_un_campo_entero_o_tecla_a_tecla() {
        let mut f = SearchForm::new();
        f.set_texto(SearchField::Content, "hola".into());
        assert_eq!(f.texto(SearchField::Content), "hola");
        assert_eq!(f.texto(SearchField::Name), "", "y solo ese campo");
        f.push_char('a');
        f.push_char('b');
        f.backspace();
        assert_eq!(
            f.texto(SearchField::Name),
            "a",
            "el activo sigue siendo el nombre"
        );
    }
}
