//! `org.norte.git-status`: la columna oficial de estado de git.
//!
//! El host abre la raíz del repositorio —el ancestro que contiene `.git`, que
//! es lo que declara el manifiesto como `location-root-marker`— y le pasa a
//! este guest un token opaco y el prefijo del directorio que el usuario está
//! mirando. Desde ahí, todo lo que hace este plugin es leer: `.git/index`,
//! los `.gitignore` que apliquen, y —solo cuando el `stat` no basta— el
//! fichero en cuestión.
//!
//! Lo que NO hace: escribir, ejecutar `git`, ni saber dónde está nada. No hay
//! rutas en este código; hay un token y caminos relativos.
//!
//! # Lo que esta columna NO puede decir, y por qué (#225, ADR 0057)
//!
//! Compara el ÁRBOL DE TRABAJO contra el índice, y nada más. Las tres
//! fronteras, dichas aquí para que nadie tenga que deducirlas del código:
//!
//! - **El estado «staged» (índice contra HEAD).** `M` significa «distinto del
//!   índice». El `git status` corto tiene dos columnas porque un fichero puede
//!   estar añadido, o staged y modificado otra vez. Distinguirlos exige leer el
//!   árbol de HEAD, o sea un lector de la base de objetos dentro de un guest
//!   `no_std`: los objetos sueltos son flujos zlib y los empaquetados piden el
//!   índice del pack. Es mucho código, y la primera versión no lo intenta.
//! - **Los submódulos.** Su entrada es un gitlink y se reconoce como tal, así
//!   que ya no se dan por borrados; pero saber si tienen cambios exige abrir el
//!   repositorio de dentro. La celda queda VACÍA, que es callar en vez de
//!   afirmar.
//! - **Una ubicación que no es `file://`.** La capacidad de ubicación no acuña
//!   token para sftp, s3, mem ni el interior de un archivo comprimido: el
//!   abridor confinado necesita un descriptor de directorio de verdad. Ahí la
//!   columna sale vacía, que es correcto y conviene tenerlo escrito — el mismo
//!   plugin PARECE roto para quien esté mirando un checkout remoto.
#![cfg_attr(target_arch = "wasm32", no_std)]

extern crate alloc;

// La capa WASM solo existe cuando se compila COMO componente: los tests del
// host compilan el mismo crate sin ella, que es lo que permite probar las
// decisiones sin un runtime wasm por medio.
#[cfg(target_arch = "wasm32")]
mod guest;
pub mod ignore;
pub mod index;
pub mod sha1;
pub mod status;

/// Id de la columna que este plugin aporta; el mismo del manifiesto.
pub const COLUMN_ID: &str = "git-status";

/// Junta los ficheros de ignores que aplican a `prefix`: el de la raíz del
/// repositorio, los de cada directorio del camino, y `.git/info/exclude`.
///
/// En ese orden a propósito: en gitignore gana la última regla que casa, y la
/// más cercana al fichero es la que manda.
pub fn load_ignores(loc: &dyn status::Location, prefix: &[u8]) -> ignore::Ignores {
    let mut ign = ignore::Ignores::default();
    if let Ok(content) = loc.read(b".git/info/exclude") {
        ign.add_file(b"", &content);
    }
    if let Ok(content) = loc.read(b".gitignore") {
        ign.add_file(b"", &content);
    }
    let mut base: alloc::vec::Vec<u8> = alloc::vec::Vec::new();
    for comp in prefix.split(|b| *b == b'/').filter(|c| !c.is_empty()) {
        if !base.is_empty() {
            base.push(b'/');
        }
        base.extend_from_slice(comp);
        let mut fichero = base.clone();
        fichero.extend_from_slice(b"/.gitignore");
        if let Ok(content) = loc.read(&fichero) {
            ign.add_file(&base, &content);
        }
    }
    ign
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeMap;
    use alloc::string::{String, ToString};
    use alloc::vec::Vec;

    struct Fake(BTreeMap<Vec<u8>, Vec<u8>>);

    impl status::Location for Fake {
        fn read(&self, rel: &[u8]) -> Result<Vec<u8>, String> {
            self.0
                .get(rel)
                .cloned()
                .ok_or_else(|| "no existe".to_string())
        }

        fn stat(&self, _rel: &[u8]) -> Result<status::Meta, String> {
            Err("no hace falta".to_string())
        }
    }

    /// El `.gitignore` más cercano gana, y `.git/info/exclude` cuenta como uno
    /// de la raíz: las tres fuentes están, y en el orden que decide.
    #[test]
    fn los_ignores_se_apilan_de_la_raiz_hacia_dentro() {
        let mut files = BTreeMap::new();
        files.insert(b".git/info/exclude".to_vec(), b"*.bak\n".to_vec());
        files.insert(b".gitignore".to_vec(), b"*.log\n".to_vec());
        files.insert(b"src/.gitignore".to_vec(), b"!guardado.log\n".to_vec());
        let fake = Fake(files);

        let ign = load_ignores(&fake, b"src/deep");
        assert!(
            ign.is_ignored(b"cualquiera.bak", false),
            "el exclude cuenta"
        );
        assert!(ign.is_ignored(b"raiz.log", false));
        assert!(
            !ign.is_ignored(b"src/guardado.log", false),
            "el .gitignore de `src` gana al de la raíz"
        );
    }

    #[test]
    fn sin_ficheros_de_ignores_no_hay_reglas() {
        let ign = load_ignores(&Fake(BTreeMap::new()), b"a/b");
        assert!(ign.is_empty());
    }
}
