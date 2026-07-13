//! NIGHTLY: provider object contra un servidor S3 REAL (MinIO) por
//! testcontainers (ADR 0016 J, spec §12). Fuera del gate de PR (exige
//! Docker): lo corre `just it-remote` desde el workflow nightly.
//!
//! Se completa en la fase 7e (roundtrip, corpus hostil, copy_native,
//! conditional write real). Cobertura pendiente que el harness in-process
//! NO da (keys largas, siembra externa, presupuesto con root, panic de
//! build_rel_path): issue #50.
#![cfg(feature = "it-s3")]
