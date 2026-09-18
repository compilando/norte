//! Organizar un directorio (fase 8 del programa WOW): un plan que MUEVE a
//! subdirectorios, y que por tanto además los crea.
//!
//! Es el hermano del renombrado por lotes, y la diferencia cabe en una
//! frase: allí el destino es un nombre, aquí es una ruta relativa. Eso
//! arrastra tres cosas que no se pueden tomar prestadas de aquél:
//!
//! - **Hay que validar la ruta.** `proposed_rel` lo escribe un tercero —un
//!   modelo o un plugin— y un `..` ahí es una escritura fuera del directorio
//!   que el humano estaba mirando. Lo comprueba
//!   [`norte_proto::methods::validar_proposed_rel`], que vive en el
//!   protocolo para que el core y los frontends apliquen la MISMA regla.
//! - **Hay que crear carpetas**, y tienen que ir en el mismo lote que los
//!   movimientos: si no, deshacer devuelve los ficheros y se olvida los
//!   directorios.
//! - **No hay directorio común.** Por eso el journal marca los movimientos
//!   con [`crate::OP_ORGANIZED`] y no con `renamed`: sin esa marca, el
//!   undo los tomaría por un lote de renombrados de un solo directorio y los
//!   desharía contra el equivocado.

use std::collections::BTreeSet;

use norte_proto::methods::{OrganizeMove, PlanHash};
use norte_proto::{Error, Segment, VPath};

use crate::hashing::hex_lower;

/// Un plan de organizar ya validado y atado a su directorio.
///
/// Que exista este tipo es lo que impide aplicar un plan que nadie revisó:
/// [`Engine::organize`](crate::Engine::organize) sólo acepta el `plan_hash`
/// que sale de aquí.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizePlan {
    /// Los movimientos, con su destino ya partido en segmentos.
    pasos: Vec<OrganizeStep>,
    /// El token que hay que devolver para aplicarlo.
    hash: PlanHash,
}

/// Un movimiento validado: de dónde sale y a dónde va, ya en segmentos.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrganizeStep {
    /// El nombre que hay ahora, dentro del directorio del plan.
    pub current: Segment,
    /// El destino, relativo al mismo directorio. Al menos un segmento; los
    /// de en medio son carpetas que puede haber que crear.
    pub rel: Vec<Segment>,
}

impl OrganizeStep {
    /// La ruta absoluta de destino, colgando de `dir`.
    #[must_use]
    pub fn destino(&self, dir: &VPath) -> VPath {
        let mut p = dir.clone();
        for s in &self.rel {
            p = p.join(s.clone());
        }
        p
    }

    /// Las carpetas que este paso necesita bajo `dir`, de la más alta a la
    /// más honda. El ÚLTIMO segmento es el fichero y no entra.
    #[must_use]
    pub fn carpetas(&self, dir: &VPath) -> Vec<VPath> {
        let mut out = Vec::new();
        let mut p = dir.clone();
        for s in self.rel.iter().take(self.rel.len().saturating_sub(1)) {
            p = p.join(s.clone());
            out.push(p.clone());
        }
        out
    }
}

impl OrganizePlan {
    /// Valida `moves` contra `dir` y ata el plan a ese directorio.
    ///
    /// Un solo destino inválido tumba el plan ENTERO, y es deliberado: un
    /// plan es una intención que un humano aprueba de una vez, y aplicar «lo
    /// que se pudo» de una propuesta que traía un `..` sería quedarse con la
    /// mitad de algo que nadie revisó. Fail-loud, como el plan de
    /// renombrado hace con los nombres hostiles.
    ///
    /// # Errors
    /// [`Error::InvalidPath`] si algún `current` no es un nombre válido o
    /// algún `proposed_rel` no pasa
    /// [`norte_proto::methods::validar_proposed_rel`]; también si dos
    /// movimientos se pisan —el mismo origen dos veces, o dos destinos
    /// iguales—, que es un plan que no se puede cumplir entero.
    pub fn bind(dir: &VPath, moves: &[OrganizeMove]) -> Result<Self, Error> {
        let mut pasos = Vec::with_capacity(moves.len());
        let mut origenes: BTreeSet<Vec<u8>> = BTreeSet::new();
        let mut destinos: BTreeSet<Vec<Vec<u8>>> = BTreeSet::new();
        for m in moves {
            let current = Segment::new(m.current.as_bytes()).map_err(|_| Error::InvalidPath)?;
            let rel = norte_proto::methods::validar_proposed_rel(&m.proposed_rel)
                .map_err(|_| Error::InvalidPath)?;
            // Un origen repetido es un plan que se contradice; dos destinos
            // iguales, uno que pierde un fichero. Las dos cosas se rechazan
            // ANTES de tocar nada: a mitad de camino ya no hay plan que
            // revisar.
            if !origenes.insert(current.as_bytes().to_vec()) {
                return Err(Error::InvalidPath);
            }
            let clave: Vec<Vec<u8>> = rel.iter().map(|s| s.as_bytes().to_vec()).collect();
            if !destinos.insert(clave) {
                return Err(Error::InvalidPath);
            }
            pasos.push(OrganizeStep { current, rel });
        }
        let hash = hash_de(dir, &pasos);
        Ok(Self { pasos, hash })
    }

    /// Los pasos validados.
    #[must_use]
    pub fn pasos(&self) -> &[OrganizeStep] {
        &self.pasos
    }

    /// El token que hay que devolver para aplicarlo.
    #[must_use]
    pub fn hash(&self) -> &PlanHash {
        &self.hash
    }

    /// Todas las carpetas que el plan necesita bajo `dir`, sin repetir y de
    /// la más alta a la más honda.
    ///
    /// El orden importa: crear `a/b` antes que `a` falla en cualquier
    /// provider que no cree padres por su cuenta, y no se le puede pedir a
    /// un provider que lo haga —la regla es que el core sepa qué creó, para
    /// poder deshacerlo—.
    #[must_use]
    pub fn carpetas(&self, dir: &VPath) -> Vec<VPath> {
        let mut vistas: BTreeSet<String> = BTreeSet::new();
        let mut out: Vec<VPath> = Vec::new();
        for paso in &self.pasos {
            for c in paso.carpetas(dir) {
                if vistas.insert(c.to_wire()) {
                    out.push(c);
                }
            }
        }
        // Por profundidad: el padre antes que el hijo. `segments().count()`
        // es el número de tramos, o sea exactamente la profundidad.
        out.sort_by_key(|p| p.segments().count());
        out
    }
}

/// El hash de un plan, atado a su directorio.
///
/// Mismo criterio que `DirPlan::bind` del renombrado —un dominio propio para
/// que este digest no pueda colisionar con otro del core sobre los mismos
/// bytes— y con los pasos dentro, para que cambiar un destino invalide el
/// token que el humano aprobó.
fn hash_de(dir: &VPath, pasos: &[OrganizeStep]) -> PlanHash {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"norte-organize-plan-dir-v1");
    alimenta(&mut h, dir.to_wire().as_bytes());
    for paso in pasos {
        alimenta(&mut h, paso.current.as_bytes());
        // Cuántos segmentos, y luego cada uno: sin la cuenta, `a/b` y `ab`
        // podrían alimentar los mismos bytes.
        alimenta(&mut h, &(paso.rel.len() as u64).to_le_bytes());
        for s in &paso.rel {
            alimenta(&mut h, s.as_bytes());
        }
    }
    // INVARIANTE: el hex en minúsculas de un sha256 son 64 dígitos, que es
    // todo el contrato de `PlanHash`.
    PlanHash::parse(&hex_lower(&h.finalize())).expect("un sha256 en hex es un PlanHash")
}

/// Alimenta un trozo con su longitud delante, para que dos trozos distintos
/// no puedan producir la misma cadena de bytes.
fn alimenta(h: &mut impl sha2::Digest, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

#[cfg(test)]
mod tests {
    use super::{OrganizeMove, OrganizePlan};
    use norte_proto::VPath;

    fn dir() -> VPath {
        VPath::parse("mem:///descargas").expect("wire")
    }

    fn mov(current: &str, rel: &str) -> OrganizeMove {
        OrganizeMove {
            current: current.to_owned(),
            proposed_rel: rel.to_owned(),
        }
    }

    /// **Un destino que se sale del directorio tumba el plan ENTERO.**
    ///
    /// Es la propiedad de seguridad de toda la fase: `proposed_rel` lo
    /// escribe un modelo o un plugin, y un `..` ahí es una escritura fuera
    /// de lo que el humano estaba mirando. Y tumba el plan entero, no sólo
    /// ese paso: aplicar «lo que se pudo» de una propuesta que traía eso
    /// sería quedarse con la mitad de algo que nadie revisó.
    #[test]
    fn un_destino_que_se_sale_tumba_el_plan_entero() {
        for malo in [
            "../fuera.txt",
            "a/../../fuera.txt",
            "/etc/passwd",
            "",
            "a//b",
            "a/",
            "./x",
        ] {
            let r = OrganizePlan::bind(&dir(), &[mov("bueno.txt", "ok/bueno.txt"), mov("x", malo)]);
            assert!(r.is_err(), "«{malo}» tendría que rechazarse");
        }
    }

    /// Dos movimientos que se pisan —mismo origen, o mismo destino— son un
    /// plan que no se puede cumplir entero, y se rechaza antes de tocar nada.
    #[test]
    fn un_plan_que_se_contradice_se_rechaza() {
        let mismo_origen = OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a"), mov("a.txt", "y/a")]);
        assert!(mismo_origen.is_err());
        let mismo_destino = OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a"), mov("b.txt", "x/a")]);
        assert!(mismo_destino.is_err());
    }

    /// Las carpetas salen sin repetir y con el PADRE ANTES QUE EL HIJO:
    /// crear `a/b` antes que `a` falla en cualquier provider que no invente
    /// los padres.
    #[test]
    fn las_carpetas_van_de_la_mas_alta_a_la_mas_honda() {
        let plan = OrganizePlan::bind(
            &dir(),
            &[
                mov("uno.pdf", "facturas/2026/marzo/uno.pdf"),
                mov("dos.pdf", "facturas/2026/abril/dos.pdf"),
                mov("tres.txt", "notas/tres.txt"),
            ],
        )
        .expect("plan válido");
        let carpetas: Vec<String> = plan
            .carpetas(&dir())
            .iter()
            .map(|p| p.to_wire().replace("mem:///descargas/", ""))
            .collect();
        assert_eq!(
            carpetas,
            vec![
                "facturas",
                "notas",
                "facturas/2026",
                "facturas/2026/marzo",
                "facturas/2026/abril"
            ],
            "sin repetir `facturas`, y cada padre antes que su hijo"
        );
    }

    /// Un destino SIN subdirectorio es un renombrado corriente, y vale: la
    /// misma pantalla sirve para ordenar y para renombrar de paso.
    #[test]
    fn un_destino_sin_carpeta_es_un_renombrado() {
        let plan = OrganizePlan::bind(&dir(), &[mov("a.txt", "b.txt")]).expect("plan válido");
        assert!(plan.carpetas(&dir()).is_empty());
        assert_eq!(
            plan.pasos()[0].destino(&dir()).to_wire(),
            "mem:///descargas/b.txt"
        );
    }

    /// El hash ATA el plan a su directorio y a sus pasos: cambiar cualquiera
    /// de las dos cosas invalida el token que el humano aprobó.
    #[test]
    fn el_hash_ata_el_plan_al_directorio_y_a_los_pasos() {
        let a = OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a.txt")]).expect("plan");
        let mismo = OrganizePlan::bind(&dir(), &[mov("a.txt", "x/a.txt")]).expect("plan");
        assert_eq!(a.hash(), mismo.hash(), "el mismo plan, el mismo token");

        let otro_dir = OrganizePlan::bind(
            &VPath::parse("mem:///otro").expect("wire"),
            &[mov("a.txt", "x/a.txt")],
        )
        .expect("plan");
        assert_ne!(a.hash(), otro_dir.hash(), "otro directorio, otro token");

        let otro_destino = OrganizePlan::bind(&dir(), &[mov("a.txt", "y/a.txt")]).expect("plan");
        assert_ne!(a.hash(), otro_destino.hash());
    }

    /// Y los segmentos van con su longitud delante, así que `a/b` y `ab` no
    /// pueden alimentar los mismos bytes.
    #[test]
    fn dos_planes_distintos_no_comparten_hash_por_concatenacion() {
        let partido = OrganizePlan::bind(&dir(), &[mov("f", "a/b")]).expect("plan");
        let junto = OrganizePlan::bind(&dir(), &[mov("f", "ab")]).expect("plan");
        assert_ne!(partido.hash(), junto.hash());
    }
}
