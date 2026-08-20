//! El rastro de navegación, que ya no vive aquí.
//!
//! [`Trail`] y [`TrailStep`] se fueron a `norte-frontend`: la pregunta que
//! responden —de dónde vengo, a dónde vuelvo— no es de terminal, y el host
//! gráfico la hace igual. Se re-exportan para no tocar los call-sites.

pub use norte_frontend::nav::{Trail, TrailStep};
