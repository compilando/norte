//! La paleta de comandos.
//!
//! El MODELO (filtrado, cursor, qué está seleccionado) vive en
//! `norte-frontend` desde la fase 4 del plan multi-frontend: son reglas de
//! presentación, y dos frontends con dos copias son dos paletas que se
//! comportan distinto sin que nadie lo note (ADR 0066, decisión D14). Este
//! módulo lo re-exporta para no tocar los call sites; sus tests se fueron con
//! el modelo, que es donde describen algo.

pub use norte_frontend::palette_state::Palette;
