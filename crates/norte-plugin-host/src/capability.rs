//! Capabilities de un plugin (ADR 0022 D4): lo que el manifiesto DECLARA y el
//! host hace cumplir. `exec` es SIEMPRE `none` (spec §7.1) — se valida al
//! parsear el manifiesto, no se representa aquí.

use serde::Deserialize;

/// Alimenta un hasher con una cadena longitud-prefijada (longitud u64 LE +
/// bytes). Sin ambigüedad por concatenación. `pub(crate)` para componer digests
/// canónicos desde otros módulos (p. ej. [`crate::Manifest::approval_digest`]).
pub(crate) fn update_str(h: &mut sha2::Sha256, s: &str) {
    use sha2::Digest;
    h.update((s.len() as u64).to_le_bytes());
    h.update(s.as_bytes());
}

/// Alimenta un hasher con una cadena OPCIONAL: presencia (`0`/`1`) + la cadena
/// longitud-prefijada. Distingue `None` de `Some("")`.
pub(crate) fn update_opt_str(h: &mut sha2::Sha256, value: Option<&str>) {
    use sha2::Digest;
    match value {
        None => h.update([0u8]),
        Some(s) => {
            h.update([1u8]);
            update_str(h, s);
        }
    }
}

/// Codifica un digest binario a hex minúsculas (64 chars para sha256).
pub(crate) fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut hex = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        // El write a un String jamás falla; el `_` no oculta un error real.
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Alcance de un permiso de FS: nada, o solo lo que el host abre y pasa (jamás
/// el FS a pelo — regla dura 9).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Scope {
    /// Sin acceso.
    #[default]
    None,
    /// Solo los recursos que el host entrega explícitamente.
    Scoped,
}

impl Scope {
    /// `true` si concede algún acceso (para pintar el badge).
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Scope::Scoped)
    }

    /// Byte canónico y estable para el digest de capabilities (issue #69). NO se
    /// usa el discriminante del enum (podría reordenarse) sino un valor fijo.
    fn digest_tag(self) -> u8 {
        match self {
            Scope::None => 0,
            Scope::Scoped => 1,
        }
    }
}

/// Permiso de red: una allow-list de hosts.
#[derive(Debug, Clone, Deserialize)]
pub struct NetCap {
    /// Hosts a los que el plugin puede conectar (exacto, sin comodines por ahora).
    pub hosts: Vec<String>,
}

/// El bloque `[capabilities]` del manifiesto, ya validado. Un permiso ausente =
/// `None`/vacío: sin syscall.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Lectura de FS.
    #[serde(default, rename = "fs-read")]
    pub fs_read: Scope,
    /// Escritura de FS.
    #[serde(default, rename = "fs-write")]
    pub fs_write: Scope,
    /// Red (allow-list de hosts); ausente = sin red.
    #[serde(default)]
    pub net: Option<NetCap>,
    /// Acceso a IA (`ai = "chat"`); ausente = sin IA. Se guarda el string
    /// crudo (los modos concretos se tipan en M4-ai).
    #[serde(default)]
    pub ai: Option<String>,
    /// `exec`: DEBE ser `none` o estar ausente. Se valida y descarta al parsear
    /// el manifiesto ([`crate::Manifest::from_toml`]); jamás se expone aquí.
    #[serde(default)]
    pub(crate) exec: Option<String>,
}

impl Capabilities {
    /// Capabilities con `fs-read=scoped` (para tests del enforcement).
    #[doc(hidden)]
    #[must_use]
    pub fn scoped_read_for_test() -> Self {
        Self {
            fs_read: Scope::Scoped,
            ..Self::default()
        }
    }

    /// Digest hex (sha256) de la forma CANÓNICA de estas capabilities (issue
    /// #69). El host lo guarda JUNTO a la aprobación del humano; si el
    /// `plugin.toml` cambia en disco tras aprobar y un `discover` posterior trae
    /// capabilities distintas, este digest deja de casar y la aprobación se trata
    /// como inexistente (re-consentimiento) — defensa contra el confused-deputy
    /// TOCTOU aprobar↔ejecutar.
    ///
    /// La forma es determinista y NO ambigua: cada campo va con su presencia
    /// (`0`/`1`) y las cadenas van longitud-prefijadas (u64 LE), de modo que dos
    /// conjuntos de capabilities distintos no puedan colisionar por concatenación
    /// (p. ej. un host `"a,b"` frente a dos hosts `"a"`,`"b"`). Los hosts de red
    /// se ordenan: es un CONJUNTO, su orden en el fichero no es semántico.
    #[must_use]
    pub fn digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        // Prefijo de dominio + versión del esquema: si algún día cambia la forma
        // canónica, los digests viejos no colisionan con los nuevos.
        h.update(b"norte-plugin-caps:v1\n");
        self.update_digest(&mut h);
        hex_lower(&h.finalize())
    }

    /// Alimenta un hasher con la forma CANÓNICA de estas capabilities, SIN
    /// finalizar (para componer un digest de mayor alcance — p. ej. el del
    /// manifiesto, [`crate::Manifest::approval_digest`]). No emite prefijo de
    /// dominio propio: lo pone quien finaliza.
    pub(crate) fn update_digest(&self, h: &mut sha2::Sha256) {
        use sha2::Digest;
        h.update([self.fs_read.digest_tag()]);
        h.update([self.fs_write.digest_tag()]);
        // net: presencia + nº de hosts + cada host longitud-prefijado. Los hosts
        // se ORDENAN y se DEDUPLICAN: es un CONJUNTO, ni el orden ni las
        // repeticiones en el fichero son semánticos.
        match &self.net {
            None => h.update([0u8]),
            Some(net) => {
                h.update([1u8]);
                let mut hosts: Vec<&str> = net.hosts.iter().map(String::as_str).collect();
                hosts.sort_unstable();
                hosts.dedup();
                h.update((hosts.len() as u64).to_le_bytes());
                for host in hosts {
                    h.update((host.len() as u64).to_le_bytes());
                    h.update(host.as_bytes());
                }
            }
        }
        // ai: presencia + cadena longitud-prefijada.
        update_opt_str(h, self.ai.as_deref());
        // exec: SIEMPRE `none`/ausente (se valida al parsear), pero entra en el
        // digest por completitud — si un futuro relajara la invariante, el cambio
        // se reflejaría en la aprobación.
        update_opt_str(h, self.exec.as_deref());
    }

    /// Etiquetas cortas de los permisos concedidos, para el badge del gestor
    /// (ADR 0022 D5): p. ej. `["fs-read", "net"]`.
    #[must_use]
    pub fn badges(&self) -> Vec<&'static str> {
        let mut out = Vec::new();
        if self.fs_read.granted() {
            out.push("fs-read");
        }
        if self.fs_write.granted() {
            out.push("fs-write");
        }
        if self.net.is_some() {
            out.push("net");
        }
        if self.ai.is_some() {
            out.push("ai");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn digest_es_estable_e_hex_de_64() {
        let caps = Capabilities::scoped_read_for_test();
        let d = caps.digest();
        assert_eq!(d.len(), 64, "sha256 hex = 64 chars: {d}");
        assert!(d.bytes().all(|b| b.is_ascii_hexdigit()));
        // Determinista: dos cálculos del mismo valor coinciden.
        assert_eq!(d, caps.digest());
    }

    #[test]
    fn digest_cambia_cuando_cambian_las_capabilities() {
        let base = Capabilities::default();
        let read = Capabilities::scoped_read_for_test();
        assert_ne!(
            base.digest(),
            read.digest(),
            "añadir fs-read debe mover el digest (re-consentimiento)"
        );

        let with_net = Capabilities {
            net: Some(NetCap {
                hosts: vec!["example.com".into()],
            }),
            ..Capabilities::default()
        };
        assert_ne!(
            base.digest(),
            with_net.digest(),
            "añadir net debe mover el digest"
        );
    }

    #[test]
    fn digest_de_net_es_por_conjunto_no_por_orden() {
        let a = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into(), "b.example".into()],
            }),
            ..Capabilities::default()
        };
        let b = Capabilities {
            net: Some(NetCap {
                hosts: vec!["b.example".into(), "a.example".into()],
            }),
            ..Capabilities::default()
        };
        assert_eq!(
            a.digest(),
            b.digest(),
            "el orden de los hosts no es semántico: mismo conjunto = mismo digest"
        );
    }

    #[test]
    fn digest_de_net_deduplica_hosts_repetidos() {
        // MINOR 2: un host repetido no cambia el conjunto de permisos, así que no
        // debe cambiar el digest respecto a declararlo una sola vez.
        let once = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into()],
            }),
            ..Capabilities::default()
        };
        let twice = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into(), "a.example".into()],
            }),
            ..Capabilities::default()
        };
        assert_eq!(once.digest(), twice.digest());
    }

    #[test]
    fn digest_no_confunde_por_concatenacion_de_hosts() {
        // Un host "a.example,b.example" NO debe colisionar con dos hosts
        // "a.example" y "b.example" (longitud-prefijado evita la ambigüedad).
        let joined = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example,b.example".into()],
            }),
            ..Capabilities::default()
        };
        let split = Capabilities {
            net: Some(NetCap {
                hosts: vec!["a.example".into(), "b.example".into()],
            }),
            ..Capabilities::default()
        };
        assert_ne!(joined.digest(), split.digest());
    }
}
