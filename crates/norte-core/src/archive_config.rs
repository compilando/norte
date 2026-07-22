//! Carga MÍNIMA de la sección `[archive]` de `norte.toml` para procesos SIN
//! el loader del TUI (#95: el daemon servía con defaults compilados). Solo la
//! capa de USUARIO (`config_dir()/norte.toml`): la capa de proyecto se ignora
//! por diseño — un repo ajeno jamás sube límites de seguridad (#95.2). Mismo
//! patrón que `PolicyConfig::load` (síncrono, arranque, fail-loud).

use norte_vfs_archive::Limits;

/// `norte.toml` visto por el daemon: SOLO `[archive]`; el resto de secciones
/// (TUI, daemon, panes…) se toleran y se ignoran.
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct NorteToml {
    archive: ArchiveSection,
}

/// La sección `[archive]` (#95.2). Campos opcionales: ausente = default
/// compilado. Tolerante a campos futuros (un daemon viejo no revienta).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
// Los nombres calcan las claves TOML de `[archive]` (contrato con el
// usuario), no se renombran por estilo.
#[allow(clippy::struct_field_names)]
struct ArchiveSection {
    max_entries: Option<u64>,
    max_decompressed_bytes: Option<u64>,
    max_nesting: Option<usize>,
}

/// Parsea los overrides de `[archive]` de un `norte.toml`. `None` = sin
/// sección o sin campos (usar defaults compilados).
///
/// # Errors
/// TOML inválido o campos de `[archive]` con tipo incorrecto — fail-loud: un
/// operador que BAJÓ límites para agentes no debe quedarse en 64 GiB por un
/// typo silencioso.
fn parse(s: &str) -> Result<Option<Limits>, toml::de::Error> {
    let cfg: NorteToml = toml::from_str(s)?;
    let a = cfg.archive;
    if a.max_entries.is_none() && a.max_decompressed_bytes.is_none() && a.max_nesting.is_none() {
        return Ok(None);
    }
    let mut limits = Limits::default();
    if let Some(n) = a.max_entries {
        // Saturación HACIA ARRIBA (solo posible en 32-bit con un valor >
        // u32::MAX): jamás recorta un límite por wrap, y no en silencio.
        limits.max_entries = usize::try_from(n).unwrap_or_else(|_| {
            tracing::warn!(
                n,
                "[archive] max_entries satura a usize::MAX en esta plataforma"
            );
            usize::MAX
        });
    }
    if let Some(b) = a.max_decompressed_bytes {
        limits.max_decompressed_bytes = b;
    }
    if let Some(n) = a.max_nesting {
        limits.max_nesting = n;
    }
    Ok(Some(limits))
}

/// Carga los overrides de `[archive]` desde `config_dir()/norte.toml`
/// (capa de usuario; ausente = `Ok(None)`). SÍNCRONA (arranque): no invocar
/// desde contexto async sin `spawn_blocking`.
///
/// # Errors
/// Error de lectura (que no sea `NotFound`) o TOML/tipos inválidos.
pub fn load_archive_limits() -> std::io::Result<Option<Limits>> {
    let path = crate::connect::config_dir().join("norte.toml");
    match std::fs::read_to_string(&path) {
        Ok(s) => parse(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sin_seccion_ni_campos_es_none() {
        assert!(parse("").expect("vacío").is_none());
        assert!(
            parse("[panes]\nratio = 50\n")
                .expect("otras secciones")
                .is_none()
        );
        assert!(parse("[archive]\n").expect("sección vacía").is_none());
    }

    #[test]
    fn overrides_se_aplican_sobre_defaults() {
        let l = parse("[archive]\nmax_entries = 100\n")
            .expect("parsea")
            .expect("hay overrides");
        assert_eq!(l.max_entries, 100);
        assert_eq!(
            l.max_decompressed_bytes,
            Limits::default().max_decompressed_bytes,
            "el campo ausente conserva el default"
        );
        let l = parse("[archive]\nmax_decompressed_bytes = 1024\n")
            .expect("parsea")
            .expect("hay overrides");
        assert_eq!(l.max_decompressed_bytes, 1024);
    }

    #[test]
    fn toml_roto_o_tipo_malo_es_error_fail_loud() {
        assert!(parse("[archive\n").is_err(), "sintaxis rota");
        assert!(
            parse("[archive]\nmax_entries = \"muchas\"\n").is_err(),
            "tipo incorrecto"
        );
    }

    #[test]
    fn secciones_ajenas_con_campos_desconocidos_se_toleran() {
        let doc = "[daemon]\nmode = \"daemon\"\n[archive]\nmax_entries = 7\n[colores]\nx = 1\n";
        let l = parse(doc).expect("tolera").expect("overrides");
        assert_eq!(l.max_entries, 7);
    }
}
