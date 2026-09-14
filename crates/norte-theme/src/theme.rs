//! [`Theme`]: un conjunto de [`Style`]s por [`Role`], más una capa de efectos
//! OPACA reservada a la GPU de la GUI (ADR 0020 D4). Se parsea desde TOML.

use std::collections::HashMap;

use serde::Deserialize;

use crate::files::{FileColors, FileKind};
use crate::role::Role;
use crate::style::Style;

/// Un tema completo. Los roles ausentes heredan su
/// [`fallback`](Role::fallback), así que un tema parcial SIEMPRE resuelve.
///
/// Deliberadamente TOLERANTE a claves desconocidas de nivel superior (no
/// `deny_unknown_fields`): así un tema con secciones de una versión más nueva
/// (p. ej. `[effects]` de la GUI, o `[files]`) no rompe un parser viejo.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct Theme {
    /// Nombre legible del tema (informativo).
    #[serde(default)]
    pub name: Option<String>,
    /// Estilos explícitos por rol; lo que falte cae al fallback.
    #[serde(default)]
    pub roles: HashMap<Role, Style>,
    /// Colores por tipo de archivo (`[files.kind]` / `[files.ext]`).
    #[serde(default)]
    pub files: FileColors,
    /// Efectos de GPU (gradientes, glow, animación…): OPACOS. Un frontend de
    /// terminal los IGNORA; la GUI de M5 los interpretará (ADR 0020 D4). Se
    /// guardan sin tipar para no romper temas cuando M5 defina el esquema.
    #[serde(default)]
    pub effects: Option<toml::Value>,
}

/// Error al cargar un tema.
#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    /// El TOML no parsea.
    #[error("tema TOML inválido: {0}")]
    Toml(#[from] toml::de::Error),
}

impl Theme {
    /// Parsea un tema desde su fuente TOML.
    ///
    /// # Errors
    /// [`ThemeError::Toml`] si el TOML no es válido.
    pub fn from_toml(src: &str) -> Result<Self, ThemeError> {
        Ok(toml::from_str(src)?)
    }

    /// Escribe el tema como TOML que [`Theme::from_toml`] vuelve a leer igual.
    ///
    /// La salida es DETERMINISTA —roles en el orden de [`Role::ALL`], clases
    /// en el de [`FileKind::ALL`], extensiones alfabéticas— y con tablas en
    /// línea, una por rol: es un fichero que alguien va a abrir y retocar a
    /// mano, así que se escribe como se escriben los presets, no como lo
    /// dejaría un serializador genérico con una sección por rol.
    ///
    /// ```
    /// use norte_theme::{Role, Theme};
    /// let nord = Theme::preset("nord").unwrap().unwrap();
    /// let vuelta = Theme::from_toml(&nord.to_toml()).unwrap();
    /// assert_eq!(vuelta.style(Role::Selection), nord.style(Role::Selection));
    /// ```
    #[must_use]
    pub fn to_toml(&self) -> String {
        use std::fmt::Write as _;
        // Escribir en un `String` no falla: los `let _ =` de abajo descartan
        // un `Result` que siempre es `Ok`.
        let mut out = String::new();
        if let Some(name) = &self.name {
            let _ = writeln!(out, "name = {}", toml::Value::from(name.as_str()));
        }
        // `[effects]` va justo detrás del nombre: sea tabla o clave suelta,
        // ahí es válido, y una clave suelta DETRÁS de una sección cambiaría
        // de dueño. `toml::to_string` de una tabla con un solo `Value` que
        // vino de parsear TOML no puede fallar; si fallara, el tema perdería
        // sus efectos y no el resto.
        if let Some(v) = &self.effects {
            let mut t = toml::Table::new();
            t.insert("effects".to_owned(), v.clone());
            if let Ok(e) = toml::to_string(&t) {
                out.push_str(&e);
            }
        }
        if !self.roles.is_empty() {
            out.push_str("\n[roles]\n");
            for role in Role::ALL {
                if let Some(s) = self.roles.get(role) {
                    let _ = writeln!(out, "{} = {}", role.as_kebab(), estilo_en_linea(*s));
                }
            }
        }
        if !self.files.kind.is_empty() {
            out.push_str("\n[files.kind]\n");
            // `regular` no está en `ALL` (no es una clase que un preset deba
            // colorear), pero un tema PUEDE traerlo y `style_for` lo lee.
            for kind in FileKind::ALL.iter().chain([&FileKind::Regular]) {
                if let Some(s) = self.files.kind.get(kind) {
                    let _ = writeln!(out, "{} = {}", kind.as_kebab(), estilo_en_linea(*s));
                }
            }
        }
        if !self.files.ext.is_empty() {
            out.push_str("\n[files.ext]\n");
            let mut exts: Vec<_> = self.files.ext.iter().collect();
            exts.sort_by(|a, b| a.0.cmp(b.0));
            for (ext, s) in exts {
                let _ = writeln!(out, "{} = {}", clave(ext), estilo_en_linea(*s));
            }
        }
        out
    }

    /// El [`Style`] efectivo de un rol: el del tema si lo define, o su
    /// [`fallback`](Role::fallback) monocromo. Un rol EXPLÍCITO del tema
    /// REEMPLAZA al fallback entero (el autor toma control total del rol), no
    /// se mezcla — así `selection = { bg = "…" }` da fondo sin heredar el
    /// `reverse` del fallback.
    #[must_use]
    pub fn style(&self, role: Role) -> Style {
        self.roles.get(&role).copied().unwrap_or(role.fallback())
    }

    /// El [`Style`] de una ENTRADA de fichero `name` (bytes, regla 1) de tipo
    /// `kind` (ADR 0020 D2). Prioridad: extensión > kind > rol `regular`.
    #[must_use]
    pub fn file_style(&self, name: &[u8], kind: FileKind) -> Style {
        self.files
            .style_for(name, kind)
            .unwrap_or_else(|| self.style(Role::Regular))
    }

    /// `true` si el tema tiene efectos declarados (los ignora un frontend de
    /// terminal; útil para que la GUI decida si activar el render de GPU).
    #[must_use]
    pub fn has_effects(&self) -> bool {
        self.effects.is_some()
    }

    /// Los nombres de los efectos declarados, si el bloque es una tabla.
    ///
    /// El bloque `[effects]` es LIBRE a propósito (ADR 0036): lo interpreta
    /// cada renderer, y este crate no sabe qué significa ninguno. Lo que sí
    /// puede decir es cómo se llaman, que es lo que un frontend necesita para
    /// enumerar los que NO sabe pintar — un tema retro que se ve idéntico se
    /// lee como roto, así que la degradación tiene que ser visible.
    ///
    /// `None` = no hay bloque, o no es una tabla. Las dos son «no hay nada
    /// que nombrar» y no se distinguen a propósito: un `[effects]` que no es
    /// una tabla es un tema mal escrito, no una lista vacía de efectos.
    ///
    /// ```
    /// use norte_theme::Theme;
    ///
    /// // El tema de fábrica declara uno: el desenfoque de los diálogos de
    /// // la ventana (spec 2026-09-11, V6). El terminal lo ignora.
    /// let t = Theme::preset_default();
    /// assert_eq!(t.effect_names().as_deref(), Some(&["backdrop".to_owned()][..]));
    ///
    /// // Uno que no declara ninguno no tiene nada que nombrar.
    /// let nord = Theme::preset("nord").unwrap().unwrap();
    /// assert!(nord.effect_names().is_none());
    /// ```
    #[must_use]
    pub fn effect_names(&self) -> Option<Vec<String>> {
        match self.effects.as_ref()? {
            toml::Value::Table(t) => Some(t.keys().cloned().collect()),
            _ => None,
        }
    }

    /// El valor de UN efecto, si es una cadena (`[effects] backdrop =
    /// "blur"`). Para el frontend que lo interprete sin depender de `toml`.
    #[must_use]
    pub fn effect_str(&self, key: &str) -> Option<&str> {
        match self.effects.as_ref()? {
            toml::Value::Table(t) => t.get(key)?.as_str(),
            _ => None,
        }
    }
}

/// `{ fg = "#…", bg = "#…", bold = true }`, con solo lo que el estilo tiene.
fn estilo_en_linea(s: Style) -> String {
    let mut partes = Vec::new();
    if let Some(fg) = s.fg {
        partes.push(format!("fg = \"{}\"", fg.to_hex()));
    }
    if let Some(bg) = s.bg {
        partes.push(format!("bg = \"{}\"", bg.to_hex()));
    }
    for (activo, nombre) in [
        (s.bold, "bold"),
        (s.dim, "dim"),
        (s.italic, "italic"),
        (s.underline, "underline"),
        (s.reverse, "reverse"),
    ] {
        if activo {
            partes.push(format!("{nombre} = true"));
        }
    }
    if partes.is_empty() {
        "{}".to_owned()
    } else {
        format!("{{ {} }}", partes.join(", "))
    }
}

/// Una clave TOML: desnuda si puede, entre comillas si no.
fn clave(k: &str) -> String {
    if !k.is_empty()
        && k.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        k.to_owned()
    } else {
        // A mano: el escritor de `toml` elige una cadena de TRIPLE comilla
        // cuando el texto lleva un salto de línea, y eso no vale como clave.
        let mut out = String::from("\"");
        for c in k.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                c if c.is_control() => {
                    use std::fmt::Write as _;
                    let _ = write!(out, "\\u{:04X}", u32::from(c));
                }
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cada preset hace ida y vuelta por `to_toml` sin perder nada: roles,
    /// clases, extensiones, nombre y efectos.
    #[test]
    fn to_toml_ida_y_vuelta_en_cada_preset() {
        for name in crate::preset_names() {
            let t = Theme::preset(name).unwrap().unwrap();
            let escrito = t.to_toml();
            let v = Theme::from_toml(&escrito)
                .unwrap_or_else(|e| panic!("[{name}] no relee: {e}\n{escrito}"));
            assert_eq!(v.name, t.name, "[{name}]");
            assert_eq!(v.roles, t.roles, "[{name}]");
            assert_eq!(v.files.kind, t.files.kind, "[{name}]");
            assert_eq!(v.files.ext, t.files.ext, "[{name}]");
            assert_eq!(v.effects, t.effects, "[{name}]");
        }
    }

    /// Un nombre con comillas o saltos, una extensión que no es clave desnuda
    /// y un `[effects]` que no es tabla: lo que un serializador a mano rompe.
    #[test]
    fn to_toml_escapa_lo_raro() {
        let mut t = Theme {
            name: Some("dice \"hola\"\ny adiós".to_owned()),
            ..Theme::default()
        };
        t.files.ext.insert("tar.gz".to_owned(), Style::new().bold());
        t.files.ext.insert("a\nb".to_owned(), Style::new().dim());
        t.files.ext.insert("a\"b\\c".to_owned(), Style::new().dim());
        t.files.kind.insert(
            FileKind::Regular,
            Style::new().fg(crate::Color::rgb(1, 2, 3)),
        );
        t.roles.insert(Role::Mark, Style::new());
        t.effects = Some(toml::Value::from("suelto"));
        let v = Theme::from_toml(&t.to_toml()).expect("relee");
        assert_eq!(v.name, t.name);
        assert_eq!(v.files.ext, t.files.ext);
        assert_eq!(v.files.kind, t.files.kind);
        assert_eq!(v.roles, t.roles);
        assert_eq!(v.effects, t.effects);
    }
}
