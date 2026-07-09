//! Codificación wire de los tipos que JSON no puede transportar tal cual.
//!
//! Las reglas de escape viven aisladas aquí (ADR 0001): nada fuera de este
//! módulo las conoce.

pub(crate) mod vpath_codec;
