//! Strings de UI por Fluent (convención de CLAUDE.md, issue #1): catálogos
//! es/en EMBEBIDOS con paridad total verificada por test. Un id ausente
//! cae al propio id (visible y greppeable), jamás panica.
#![forbid(unsafe_code)]

use std::sync::OnceLock;

use fluent::{FluentArgs, FluentResource, concurrent::FluentBundle};
use unic_langid::LanguageIdentifier;

/// Idiomas soportados.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Lang {
    /// Español.
    Es,
    /// Inglés (fallback).
    En,
}

impl Lang {
    /// Negocia desde un valor tipo `LANG`/`LC_MESSAGES` (`es_ES.UTF-8`).
    /// Desconocido o ausente → inglés.
    #[must_use]
    pub fn negotiate(env: Option<&str>) -> Self {
        match env {
            Some(v) if v.to_ascii_lowercase().starts_with("es") => Self::Es,
            _ => Self::En,
        }
    }

    /// Negocia desde el entorno del proceso: `NORTE_LANG` >
    /// `LC_ALL` > `LC_MESSAGES` > `LANG`.
    #[must_use]
    pub fn from_env() -> Self {
        for var in ["NORTE_LANG", "LC_ALL", "LC_MESSAGES", "LANG"] {
            if let Ok(v) = std::env::var(var)
                && !v.is_empty()
            {
                return Self::negotiate(Some(&v));
            }
        }
        Self::En
    }

    fn ftl(self) -> &'static str {
        match self {
            Self::Es => include_str!("../i18n/es.ftl"),
            Self::En => include_str!("../i18n/en.ftl"),
        }
    }

    fn langid(self) -> LanguageIdentifier {
        match self {
            Self::Es => "es".parse().expect("langid constante"),
            Self::En => "en".parse().expect("langid constante"),
        }
    }
}

fn bundle(lang: Lang) -> &'static FluentBundle<FluentResource> {
    static ES: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    static EN: OnceLock<FluentBundle<FluentResource>> = OnceLock::new();
    let cell = match lang {
        Lang::Es => &ES,
        Lang::En => &EN,
    };
    cell.get_or_init(|| {
        // Los .ftl embebidos los valida la suite: un error aquí es build
        // roto, no runtime del usuario.
        let res = FluentResource::try_new(lang.ftl().to_owned()).unwrap_or_else(|(res, _)| res);
        let mut b = FluentBundle::new_concurrent(vec![lang.langid()]);
        let _ = b.add_resource(res);
        // Sin marcas bidi de aislamiento: la UI es un terminal.
        b.set_use_isolating(false);
        b
    })
}

/// Idioma global del proceso (lo fija el frontend al arrancar).
static GLOBAL: OnceLock<Lang> = OnceLock::new();

/// Fija el idioma global. Solo la PRIMERA llamada gana; `false` si ya
/// estaba fijado (o usado) con otro valor.
pub fn force(lang: Lang) -> bool {
    GLOBAL.set(lang).is_ok() || GLOBAL.get() == Some(&lang)
}

fn global() -> Lang {
    *GLOBAL.get_or_init(Lang::from_env)
}

/// El idioma global vigente: el que [`force`] fijó, o el del entorno si nadie
/// lo fijó.
///
/// Existe para los callers que necesitan traducir con [`t_in`] en el idioma
/// que [`t`] usaría — un resolver que guarda el idioma en un campo, por
/// ejemplo. Sin esto tenían que re-derivar la negociación por su cuenta, y dos
/// derivaciones del mismo hecho acaban discrepando: la UI en un idioma y una
/// tabla dentro de ella en otro.
///
/// OJO: leerlo FIJA el idioma si nadie lo había fijado (`get_or_init`), igual
/// que traducir. Llamarlo antes de [`force`] hace que ese `force` posterior
/// devuelva `false` salvo que coincida.
///
/// ```
/// let lang = norte_i18n::active();
/// assert_eq!(norte_i18n::t("help-title"), norte_i18n::t_in(lang, "help-title"));
/// ```
#[must_use]
pub fn active() -> Lang {
    global()
}

/// Traduce `id` en el idioma global.
#[must_use]
pub fn t(id: &str) -> String {
    t_in(global(), id)
}

/// Traduce `id` con args en el idioma global.
#[must_use]
pub fn ta(id: &str, args: &[(&str, &str)]) -> String {
    ta_in(global(), id, args)
}

/// Traduce `id` en un idioma concreto (tests y previews).
#[must_use]
pub fn t_in(lang: Lang, id: &str) -> String {
    ta_in(lang, id, &[])
}

/// Traduce con args en un idioma concreto. Id ausente → el propio id.
#[must_use]
pub fn ta_in(lang: Lang, id: &str, args: &[(&str, &str)]) -> String {
    let b = bundle(lang);
    let Some(msg) = b.get_message(id) else {
        return id.to_owned();
    };
    let Some(pattern) = msg.value() else {
        return id.to_owned();
    };
    let mut fargs = FluentArgs::new();
    for (k, v) in args {
        fargs.set(*k, *v);
    }
    let mut errors = Vec::new();
    b.format_pattern(pattern, Some(&fargs), &mut errors)
        .into_owned()
}

/// Todos los ids de mensaje de un locale (para el test de paridad).
#[must_use]
pub fn message_ids(lang: Lang) -> Vec<String> {
    use fluent_syntax::ast::Entry;
    let res = FluentResource::try_new(lang.ftl().to_owned()).unwrap_or_else(|(res, _)| res);
    res.entries()
        .filter_map(|e| match e {
            Entry::Message(m) => Some(m.id.name.to_owned()),
            _ => None,
        })
        .collect()
}

#[cfg(test)]
mod sin_duplicados {
    use std::collections::BTreeSet;

    /// Ninguna clave se define dos veces.
    ///
    /// Fluent se queda con la PRIMERA definición y tira la segunda EN
    /// SILENCIO, así que un duplicado es una traducción que alguien escribió,
    /// que el fichero enseña, y que nadie va a leer nunca. Había uno
    /// (`layout-picker-factory`, con dos textos distintos en inglés) y lo
    /// encontró una auditoría, no el catálogo.
    #[test]
    fn ninguna_clave_se_define_dos_veces() {
        for (lang, fuente) in [
            ("en", include_str!("../i18n/en.ftl")),
            ("es", include_str!("../i18n/es.ftl")),
        ] {
            let mut vistas: BTreeSet<&str> = BTreeSet::new();
            let mut repetidas: Vec<&str> = Vec::new();
            for linea in fuente.lines() {
                // Una definición empieza en la columna cero; una
                // continuación va indentada y un comentario lleva `#`.
                let Some((id, _)) = linea.split_once(" = ") else {
                    continue;
                };
                if id.starts_with([' ', '#', '.', '*', '[']) || id.is_empty() {
                    continue;
                }
                if !vistas.insert(id) {
                    repetidas.push(id);
                }
            }
            assert!(
                repetidas.is_empty(),
                "{lang}.ftl define dos veces: {repetidas:?} — Fluent se queda \
                 con la primera y tira la otra sin decir nada"
            );
        }
    }
}
