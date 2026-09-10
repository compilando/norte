//! El asistente de primer arranque (spec 2026-09-10): tres preguntas —el
//! preset de teclas, el tema y los iconos— cuando no hay `norte.toml` de
//! usuario. Puro: qué paso, qué filas, qué se eligió. Cada frontend lo
//! pinta y escribe lo elegido por su propio camino de ajustes.
//!
//! Esc en cualquier paso es «no volver a preguntar»: el frontend escribe un
//! fichero con lo que ya tenía, y la existencia del fichero es la marca.

use norte_i18n::{Lang, t_in};

/// Los tres pasos, en orden.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Qué preset de teclas.
    Preset,
    /// Qué tema.
    Theme,
    /// Si la fuente pinta los iconos.
    Icons,
}

impl Step {
    /// `1`..=`3`, para el título.
    #[must_use]
    pub fn number(self) -> u8 {
        match self {
            Self::Preset => 1,
            Self::Theme => 2,
            Self::Icons => 3,
        }
    }

    fn index(self) -> usize {
        usize::from(self.number() - 1)
    }
}

/// Lo elegido. `None` = el lector no llegó a ese paso.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Choices {
    /// El preset de teclas.
    pub preset: Option<String>,
    /// El tema.
    pub theme: Option<String>,
    /// `Some(true)` = los iconos se ven (emoji); `Some(false)` = ASCII.
    pub icons: Option<bool>,
}

/// Qué pasó al confirmar o al salir.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Otro paso por delante.
    Continue,
    /// El último paso contestado: escribir esto.
    Done(Choices),
    /// El lector salió: no volver a preguntar, sin cambiar nada. Lleva el
    /// tema VIGENTE para que el frontend escriba exactamente ese y el
    /// fichero exista — nunca `default` sobre un tema que ya había
    /// (revisión B1).
    Dismissed {
        /// El tema con el que se abrió el asistente.
        keep_theme: String,
    },
}

/// El asistente.
#[derive(Debug, Clone)]
pub struct Wizard {
    step: Step,
    presets: Vec<String>,
    themes: Vec<String>,
    cursor: [usize; 3],
    choices: Choices,
    /// El tema con el que se abrió: lo que se conserva al salir con Esc.
    current_theme: String,
}

impl Wizard {
    /// Con los presets y los temas que hay. El cursor arranca en el preset
    /// y el tema VIGENTES, para que Enter sin mirar deje todo como estaba.
    #[must_use]
    pub fn new(
        presets: &[&str],
        themes: &[&str],
        current_preset: &str,
        current_theme: &str,
    ) -> Self {
        let presets: Vec<String> = presets.iter().map(|s| (*s).to_owned()).collect();
        let themes: Vec<String> = themes.iter().map(|s| (*s).to_owned()).collect();
        let p = presets
            .iter()
            .position(|s| s == current_preset)
            .unwrap_or(0);
        let t = themes.iter().position(|s| s == current_theme).unwrap_or(0);
        Self {
            step: Step::Preset,
            presets,
            themes,
            cursor: [p, t, 0],
            choices: Choices::default(),
            current_theme: current_theme.to_owned(),
        }
    }

    /// El paso actual.
    #[must_use]
    pub fn step(&self) -> Step {
        self.step
    }

    /// El título del paso, en el idioma dado: `norte · 1/3 · Keys`.
    #[must_use]
    pub fn title(&self, lang: Lang) -> String {
        let clave = match self.step {
            Step::Preset => "wizard-step-preset",
            Step::Theme => "wizard-step-theme",
            Step::Icons => "wizard-step-icons",
        };
        format!(
            "{} · {}/3 · {}",
            t_in(lang, "wizard-title"),
            self.step.number(),
            t_in(lang, clave)
        )
    }

    /// La pregunta del paso, en el idioma dado.
    #[must_use]
    pub fn question(&self, lang: Lang) -> String {
        let clave = match self.step {
            Step::Preset => "wizard-ask-preset",
            Step::Theme => "wizard-ask-theme",
            Step::Icons => "wizard-ask-icons",
        };
        t_in(lang, clave)
    }

    /// Las filas del paso, en el idioma dado: cada preset con su línea,
    /// los temas por nombre, y sí/no para los iconos.
    #[must_use]
    pub fn rows(&self, lang: Lang) -> Vec<String> {
        match self.step {
            Step::Preset => self
                .presets
                .iter()
                .map(|p| {
                    let clave = format!("wizard-preset-{p}");
                    let linea = t_in(lang, &clave);
                    if linea == clave {
                        p.clone()
                    } else {
                        format!("{p} — {linea}")
                    }
                })
                .collect(),
            Step::Theme => self.themes.clone(),
            Step::Icons => vec![
                t_in(lang, "wizard-icons-yes"),
                t_in(lang, "wizard-icons-no"),
            ],
        }
    }

    /// Qué fila está elegida en el paso actual.
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor[self.step.index()]
    }

    fn len(&self) -> usize {
        match self.step {
            Step::Preset => self.presets.len(),
            Step::Theme => self.themes.len(),
            Step::Icons => 2,
        }
    }

    /// Sube una fila (tope arriba).
    pub fn up(&mut self) {
        let i = self.step.index();
        self.cursor[i] = self.cursor[i].saturating_sub(1);
    }

    /// Baja una fila (tope abajo).
    pub fn down(&mut self) {
        let i = self.step.index();
        if self.cursor[i] + 1 < self.len() {
            self.cursor[i] += 1;
        }
    }

    /// Pone el cursor en `row`, si existe: lo que hace un clic.
    pub fn select(&mut self, row: usize) {
        if row < self.len() {
            self.cursor[self.step.index()] = row;
        }
    }

    /// El tema bajo el cursor mientras se elige tema: para la vista previa
    /// en vivo. `None` en los otros pasos.
    #[must_use]
    pub fn preview_theme(&self) -> Option<&str> {
        (self.step == Step::Theme)
            .then(|| self.themes.get(self.cursor()).map(String::as_str))
            .flatten()
    }

    /// Enter: guarda la fila y pasa al siguiente paso, o termina.
    pub fn confirm(&mut self) -> Outcome {
        match self.step {
            Step::Preset => {
                self.choices.preset = self.presets.get(self.cursor()).cloned();
                self.step = Step::Theme;
                Outcome::Continue
            }
            Step::Theme => {
                self.choices.theme = self.themes.get(self.cursor()).cloned();
                self.step = Step::Icons;
                Outcome::Continue
            }
            Step::Icons => {
                self.choices.icons = Some(self.cursor() == 0);
                Outcome::Done(self.choices.clone())
            }
        }
    }

    /// Backspace: el paso anterior, si lo hay.
    pub fn back(&mut self) {
        self.step = match self.step {
            Step::Preset | Step::Theme => Step::Preset,
            Step::Icons => Step::Theme,
        };
    }

    /// Esc: salir sin cambiar nada y no volver a preguntar. Lleva el tema
    /// vigente para que quien escribe conserve exactamente ese.
    #[must_use]
    pub fn dismiss(&self) -> Outcome {
        Outcome::Dismissed {
            keep_theme: self.current_theme.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w() -> Wizard {
        Wizard::new(&["orthodox", "vim"], &["default", "nord"], "vim", "nord")
    }

    /// Arranca en lo vigente; Enter tres veces devuelve lo elegido; el
    /// cursor de cada paso es suyo; Backspace vuelve.
    #[test]
    fn tres_pasos_y_lo_elegido() {
        let mut w = w();
        assert_eq!(w.step(), Step::Preset);
        assert_eq!(w.cursor(), 1, "arranca en el preset vigente");
        assert!(w.title(Lang::En).contains("1/3"));
        assert!(w.rows(Lang::En)[0].starts_with("orthodox"));
        w.up();
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(w.step(), Step::Theme);
        assert_eq!(
            w.preview_theme(),
            Some("nord"),
            "el tema vigente, bajo el cursor"
        );
        w.up();
        assert_eq!(w.preview_theme(), Some("default"));
        w.back();
        assert_eq!(
            (w.step(), w.cursor()),
            (Step::Preset, 0),
            "cada paso conserva su cursor"
        );
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(w.confirm(), Outcome::Continue);
        assert_eq!(w.step(), Step::Icons);
        assert_eq!(w.rows(Lang::Es).len(), 2);
        w.down();
        w.down();
        assert_eq!(w.cursor(), 1, "tope abajo");
        assert_eq!(
            w.confirm(),
            Outcome::Done(Choices {
                preset: Some("orthodox".into()),
                theme: Some("default".into()),
                icons: Some(false),
            })
        );
        assert_eq!(
            w.dismiss(),
            Outcome::Dismissed {
                keep_theme: "nord".into()
            },
            "salir conserva el tema con el que se abrió, no `default`"
        );
    }

    /// Un clic fuera de las filas no mueve nada; uno dentro, sí.
    #[test]
    fn select_solo_dentro() {
        let mut w = w();
        w.select(7);
        assert_eq!(w.cursor(), 1);
        w.select(0);
        assert_eq!(w.cursor(), 0);
    }
}
