//! `provider_contract!` verde sobre `MemProvider` en tres configuraciones:
//! el contrato completo con capabilities distintas ejercita también los
//! auto-skips (case-sensitive vs insensitive, con y sin `SERVER_COPY`).

// `CapabilityFlags` llega al scope de cada invocación vía los imports del
// módulo generado (norte_proto re-exportado por la macro).
use norte_testkit::MemProvider;

fn hostile() -> Vec<Vec<u8>> {
    norte_testkit::corpus::hostile_names()
        .into_iter()
        .map(|n| n.bytes)
        .collect()
}

norte_vfs::provider_contract! {
    mod mem_unix_like,
    factory: MemProvider::new(),
    root: MemProvider::root(),
    hostile_names: hostile(),
}

norte_vfs::provider_contract! {
    mod mem_case_insensitive,
    factory: MemProvider::with_flags(
        CapabilityFlags::RENAME_ATOMIC | CapabilityFlags::CASE_PRESERVING,
    ),
    root: MemProvider::root(),
    hostile_names: hostile(),
}

norte_vfs::provider_contract! {
    mod mem_server_copy,
    factory: MemProvider::with_flags(
        CapabilityFlags::SERVER_COPY | CapabilityFlags::CASE_SENSITIVE,
    ),
    root: MemProvider::root(),
    hostile_names: hostile(),
}
