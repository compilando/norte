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

/// Alimenta un hasher con un `i64` OPCIONAL: presencia (`0`/`1`) + 8 bytes LE.
/// Usado por `[config.<key>]` (P2) para `min`/`max` de las claves `int`.
pub(crate) fn update_opt_i64(h: &mut sha2::Sha256, value: Option<i64>) {
    use sha2::Digest;
    match value {
        None => h.update([0u8]),
        Some(v) => {
            h.update([1u8]);
            h.update(v.to_le_bytes());
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

/// Escritura de FS (ADR 0101): nada, o una lista CERRADA de nombres de
/// fichero —sidecars— que un `hook` puede pedir que el host escriba junto a
/// lo que cambió. El host los escribe como actor `plugin`, por el policy
/// engine y por el journal; el guest no ve rutas ni abre nada.
///
/// `fs-write = "scoped"` fue un valor reservado que nadie honraba, y desde
/// ADR 0088 eso se rechaza al parsear: la variante [`Self::Reserved`] existe
/// para que el rechazo diga qué se escribió, no para concederlo.
#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
#[serde(untagged)]
pub enum FsWriteCap {
    /// Sin escritura.
    #[default]
    #[serde(skip)]
    None,
    /// Una cadena (`"scoped"` u otra): se rechaza al validar el manifiesto.
    Reserved(String),
    /// `fs-write = { sidecar = ["a", "b"] }`: los nombres, tal cual.
    Sidecar {
        /// Nombres de fichero, un segmento cada uno. Validados en el
        /// manifiesto, no aquí.
        sidecar: Vec<String>,
    },
}

impl FsWriteCap {
    /// `true` si concede algún acceso (para pintar el badge).
    #[must_use]
    pub fn granted(&self) -> bool {
        matches!(self, FsWriteCap::Sidecar { sidecar } if !sidecar.is_empty())
    }

    /// Los nombres de sidecar concedidos; vacío si no hay escritura.
    #[must_use]
    pub fn sidecar_names(&self) -> &[String] {
        match self {
            FsWriteCap::Sidecar { sidecar } => sidecar,
            _ => &[],
        }
    }

    /// Byte canónico + contenido para el digest. `None` digesta EXACTAMENTE
    /// lo que digestaba `fs-write` ausente antes de ADR 0101 (el byte 0), así
    /// que ninguna aprobación existente se mueve. `Sidecar` lleva un byte
    /// nuevo y los nombres, en orden: cambiar qué puede escribir un plugin es
    /// cambiar lo aprobado.
    fn update_digest(&self, h: &mut sha2::Sha256) {
        use sha2::Digest;
        match self {
            FsWriteCap::None => h.update([0u8]),
            // Nunca llega al digest: se rechaza antes. El byte existe para
            // que, si llegara, no colisionara con `None`.
            FsWriteCap::Reserved(_) => h.update([1u8]),
            FsWriteCap::Sidecar { sidecar } => {
                h.update([2u8]);
                h.update((sidecar.len() as u64).to_le_bytes());
                for n in sidecar {
                    update_str(h, n);
                }
            }
        }
    }
}

/// Acceso de UBICACIÓN (ADR 0057): nada, o lectura bajo el token opaco que el
/// host entrega al pintar una columna.
///
/// Vocabulario CERRADO, como `exec`: un valor que no esté aquí es un
/// manifiesto inválido, no una capacidad que se ignora en silencio. Lo que se
/// concede es leer BAJO un directorio que el host abrió y confinó — el guest
/// jamás recibe la ruta, así que esto no abre la regla 9: la mantiene con un
/// permiso propio y visible al aprobar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LocationCap {
    /// Sin acceso a la ubicación.
    #[default]
    None,
    /// Lectura (`read`/`stat`/`list`) bajo el token.
    Read,
}

impl LocationCap {
    /// `true` si concede algún acceso (para pintar el badge).
    #[must_use]
    pub fn granted(self) -> bool {
        matches!(self, Self::Read)
    }

    /// Byte canónico y estable para el digest, igual que [`Scope::digest_tag`].
    fn digest_tag(self) -> u8 {
        match self {
            Self::None => 0,
            Self::Read => 1,
        }
    }
}

/// Permiso de red: una allow-list de hosts.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct NetCap {
    /// Hosts a los que el plugin puede conectar por TCP SALIENTE (exacto, sin
    /// comodines). Una entrada `ip:puerto` autoriza SOLO ese puerto; una de solo
    /// `ip` autoriza CUALQUIER puerto de ese host (necesario para el FTP pasivo,
    /// que negocia puertos de datos dinámicos) — el humano lo ve al aprobar. Sin
    /// DNS: se conecta por IP (resolución de hostnames = stage 3b, #30).
    pub hosts: Vec<String>,
}

/// El bloque `[capabilities]` del manifiesto, ya validado. Un permiso ausente =
/// `None`/vacío: sin syscall.
/// `PartialEq` no es cosmético: es lo que deja a un pool de instancias
/// comprobar que la instancia que va a reutilizar tiene EXACTAMENTE los
/// permisos que el catálogo acaba de resolver (#224). Sin esa comparación, un
/// consentimiento retirado tardaría en surtir efecto lo que tardase el TTL del
/// pool, que es una latencia inaceptable para un permiso.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Capabilities {
    /// Lectura de FS.
    #[serde(default, rename = "fs-read")]
    pub fs_read: Scope,
    /// Escritura de FS: los sidecars que un `hook` puede pedir (ADR 0101).
    #[serde(default, rename = "fs-write")]
    pub fs_write: FsWriteCap,
    /// Red (allow-list de hosts); ausente = sin red.
    #[serde(default)]
    pub net: Option<NetCap>,
    /// Acceso a IA (`ai = "chat"`); ausente = sin IA. Se guarda el string
    /// crudo (los modos concretos se tipan en M4-ai).
    #[serde(default)]
    pub ai: Option<String>,
    /// Ubicación (`location = "read"`); ausente = sin ubicación (ADR 0057).
    #[serde(default)]
    pub location: LocationCap,
    /// Marcador de RAÍZ DE PROYECTO (`location-root-marker = ".git"`).
    ///
    /// Con él, el host no abre el directorio que se está listando sino el
    /// ANCESTRO más cercano que contenga una entrada con ese nombre — y le
    /// dice al guest qué prefijo mira el usuario. Sin él, la raíz es el
    /// directorio visible.
    ///
    /// Existe porque la confinación es real: un token no puede subir (`..` lo
    /// rechaza el kernel), así que un plugin que necesita el fichero de
    /// control de un proyecto —`.git/index`, `Cargo.toml`, `.hg`— solo podría
    /// trabajar cuando el usuario está justo en la raíz. Lo que se concede
    /// sigue siendo VISIBLE al aprobar: el nombre del marcador se enseña con
    /// el badge, y subir de más se corta en las raíces protegidas y en un
    /// tope de niveles.
    #[serde(default, rename = "location-root-marker")]
    pub location_root_marker: Option<String>,
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

    /// Capabilities SOLO con red: un allow-list de `hosts` a los que el guest
    /// puede conectar (#30 stage 3). El resto de permisos quedan en su cero
    /// (sin fs, sin ai, sin exec). Lo usa el wiring de un provider de red y sus
    /// tests; el `exec` privado impide construir el struct desde fuera.
    #[must_use]
    pub fn with_net(hosts: Vec<String>) -> Self {
        Self {
            net: Some(NetCap { hosts }),
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
        self.fs_write.update_digest(h);
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
        // location (ADR 0057): entra en el digest SOLO cuando se concede.
        //
        // El orden importa. Emitirla siempre movería el digest de todos los
        // manifiestos que NO la piden, y eso resetea todas las aprobaciones
        // humanas ya dadas — el mismo pinchazo que P2 dejó pineado en
        // `manifest_sin_config_digesta_identico_a_pre_p2`. Emitirla solo
        // cuando se pide conserva esas aprobaciones Y sigue exigiendo una
        // nueva a quien pida la capacidad: es la propiedad que hace falta, y
        // la ausencia sigue siendo inequívoca porque lo anterior (el `exec`
        // opcional) ya se autodelimita.
        if self.location.granted() {
            h.update([b'L', self.location.digest_tag()]);
            // El marcador entra CON la capacidad: cambiar `.git` por otra cosa
            // cambia qué directorio se abre, así que exige aprobar otra vez.
            update_opt_str(h, self.location_root_marker.as_deref());
        }
        // exec: SIEMPRE `none`/ausente (se valida al parsear), pero entra en el
        // digest por completitud — si un futuro relajara la invariante, el cambio
        // se reflejaría en la aprobación.
        update_opt_str(h, self.exec.as_deref());
    }

    /// Etiquetas cortas de los permisos concedidos, para el badge del gestor
    /// (ADR 0022 D5): p. ej. `["fs-read", "net"]`.
    ///
    /// La de ubicación DICE EL MARCADOR cuando lo hay
    /// (`location-root:.git`, #241): con `location` a secas, quien aprueba lee
    /// «puede leer donde estoy mirando», y lo que concede es «puede leer el
    /// ancestro más cercano que contenga esto» — que en un repositorio son
    /// todos los ficheros del proyecto, no la carpeta que está abierta. El
    /// permiso más ancho es el que hay que nombrar.
    #[must_use]
    pub fn badges(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        if self.fs_read.granted() {
            out.push("fs-read".to_owned());
        }
        // Un badge POR NOMBRE: lo que el humano aprueba es qué ficheros
        // puede escribir el plugin, y «fs-write» a secas no lo dice.
        for n in self.fs_write.sidecar_names() {
            out.push(format!("fs-write:{n}"));
        }
        if self.net.is_some() {
            out.push("net".to_owned());
        }
        if self.ai.is_some() {
            out.push("ai".to_owned());
        }
        if self.location.granted() {
            match &self.location_root_marker {
                Some(marker) => out.push(format!("location-root:{marker}")),
                None => out.push("location".to_owned()),
            }
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
