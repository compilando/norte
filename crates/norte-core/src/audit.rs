//! Audit del journal (M3-5, ADR 0025): export determinista (JSONL/CSV) y
//! anclas HMAC del head de la cadena.
//!
//! **Garantía honesta de las anclas.** El hash-chain keyless del journal no
//! resiste a un atacante con escritura en la DB (#63): reescritura total,
//! truncación de cola y rollback pasan `verify_chain`. Un ancla
//! `HMAC-SHA256(key, "norte-anchor-v1" ‖ seq ‖ head)` con la clave en el
//! keyring del SO acota esa ventana: REESCRIBIR la historia cubierta por un
//! ancla exige ADEMÁS la clave. Lo que la clave NO protege: el propio
//! fichero de anclas vive en el mismo dir — un atacante con escritura de
//! ficheros puede BORRARLO, recortarle líneas (las anteriores siguen siendo
//! MACs válidos) o restaurar un snapshot coherente del PAR (DB + anclas),
//! todo SIN la clave. Por eso `verify` reporta la COBERTURA (hasta qué seq
//! llegan las anclas) y trata la ausencia de anclas como fallo salvo opt-out
//! explícito; la copia EXTERNA del fichero de anclas (otro host, log
//! remoto) es lo que convierte el recorte en detectable. Tampoco cubre:
//! atacante con acceso al keyring, ni mutaciones posteriores al último
//! ancla. El core NO conoce el keyring: la clave entra como bytes (la
//! resuelve el CLI vía `norte-connect`, regla 10).

use hmac::{Hmac, Mac};
use serde::Serialize;
use sha2::Sha256;

use crate::journal::JournalEntry;

type HmacSha256 = Hmac<Sha256>;

/// Un ancla: el head `(seq, entry_hash)` de la cadena en el momento de
/// anclar. Se persiste como UNA línea JSON en `journal-anchors.jsonl`
/// (append-only) junto a su MAC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Anchor {
    /// `seq` del head anclado.
    pub seq: i64,
    /// `entry_hash` del head anclado.
    pub head: [u8; 32],
}

/// Veredicto de UNA línea de anclas contra la cadena actual.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AnchorVerdict {
    /// MAC válido y el hash de la cadena en ese `seq` coincide.
    Ok(Anchor),
    /// La línea no parsea (fichero dañado o formato futuro).
    BadLine,
    /// MAC inválido: el ancla no la produjo esta clave (fabricada o clave
    /// rotada sin re-anclar).
    BadMac,
    /// La cadena YA NO TIENE ese `seq`: truncación/rollback por detrás del
    /// ancla.
    MissingSeq(Anchor),
    /// El `seq` existe pero su hash difiere: la historia se reescribió.
    HashMismatch(Anchor),
}

/// Context string del MAC: separación de dominio + versión del formato. Si
/// la clave se reutilizara para otro MAC, o el formato cambia, no hay
/// confusión cross-protocol ni migración ambigua.
const ANCHOR_CONTEXT: &[u8] = b"norte-anchor-v1";

/// `HMAC-SHA256(key, "norte-anchor-v1" ‖ seq_le ‖ head)`. Campos de longitud
/// FIJA (15+8+32): sin ambigüedad de concatenación. Deuda anotada: sin
/// identidad del journal en el MAC, un ancla de OTRO perfil (`$NORTE_CONFIG_DIR`)
/// del mismo usuario es MAC-válida contra este — produce `MissingSeq`/
/// `HashMismatch` (falsa alarma), no bypass.
fn anchor_mac(key: &[u8], seq: i64, head: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("HMAC acepta cualquier longitud de clave");
    mac.update(ANCHOR_CONTEXT);
    mac.update(&seq.to_le_bytes());
    mac.update(head);
    mac.finalize().into_bytes().into()
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::new(), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, chunk) in s.as_bytes().chunks_exact(2).enumerate() {
        let hi = char::from(chunk[0]).to_digit(16)?;
        let lo = char::from(chunk[1]).to_digit(16)?;
        out[i] = u8::try_from(hi * 16 + lo).ok()?;
    }
    Some(out)
}

/// Serializa un ancla como línea JSONL con su MAC (sin `\n` final).
#[must_use]
pub fn anchor_line(key: &[u8], anchor: &Anchor) -> String {
    let mac = anchor_mac(key, anchor.seq, &anchor.head);
    format!(
        r#"{{"seq":{},"head":"{}","mac":"{}"}}"#,
        anchor.seq,
        hex(&anchor.head),
        hex(&mac)
    )
}

/// Verifica UNA línea de anclas: parseo → MAC → contraste con el hash que la
/// cadena tiene HOY en ese `seq` (`None` = el seq ya no existe).
///
/// El MAC se comprueba ANTES de mirar la cadena: una línea fabricada sin la
/// clave jamás llega a acusar a la cadena.
///
/// # Panics
/// Nunca: HMAC acepta claves de cualquier longitud (el `expect` es la
/// invariante del constructor de la API de `RustCrypto`).
#[must_use]
pub fn verify_anchor_line(key: &[u8], line: &str, hash_at_seq: Option<[u8; 32]>) -> AnchorVerdict {
    #[derive(serde::Deserialize)]
    struct Line {
        seq: i64,
        head: String,
        mac: String,
    }
    let Ok(parsed) = serde_json::from_str::<Line>(line) else {
        return AnchorVerdict::BadLine;
    };
    let (Some(head), Some(mac)) = (unhex::<32>(&parsed.head), unhex::<32>(&parsed.mac)) else {
        return AnchorVerdict::BadLine;
    };
    // Comparación en tiempo constante (hmac::Mac::verify_slice).
    let mut check = HmacSha256::new_from_slice(key).expect("HMAC acepta cualquier longitud");
    check.update(ANCHOR_CONTEXT);
    check.update(&parsed.seq.to_le_bytes());
    check.update(&head);
    if check.verify_slice(&mac).is_err() {
        return AnchorVerdict::BadMac;
    }
    let anchor = Anchor {
        seq: parsed.seq,
        head,
    };
    match hash_at_seq {
        None => AnchorVerdict::MissingSeq(anchor),
        Some(h) if h == head => AnchorVerdict::Ok(anchor),
        Some(_) => AnchorVerdict::HashMismatch(anchor),
    }
}

/// Resultado de verificar TODO el fichero de anclas contra un snapshot de la
/// cadena (regla 7: la orquestación vive en el core, el CLI solo traduce).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct AnchorsReport {
    /// Anclas revisadas (líneas no vacías).
    pub checked: u64,
    /// Veredictos NO-Ok, con su número de línea (1-indexado).
    pub bad: Vec<(usize, AnchorVerdict)>,
    /// `seq` más alto entre las anclas Ok: hasta AHÍ llega la cobertura de
    /// las anclas. Todo lo posterior en la cadena está SIN anclar — y un
    /// recorte del fichero de anclas se manifiesta como cobertura que
    /// retrocede (por eso `verify` la imprime SIEMPRE).
    pub max_ok_seq: Option<i64>,
}

/// Verifica cada línea de `anchors_text` contra un SNAPSHOT de la cadena
/// (mapa `seq → entry_hash`, tomado de [`crate::Journal::entries`] tras un
/// [`crate::Journal::verify_chain`] `Intact` — una sola lectura, veredicto
/// coherente). El MAC decide ANTES de consultar el snapshot: una línea
/// fabricada sin la clave jamás llega a acusar a la cadena.
#[must_use]
pub fn verify_anchors<S: std::hash::BuildHasher>(
    key: &[u8],
    anchors_text: &str,
    hash_by_seq: &std::collections::HashMap<i64, [u8; 32], S>,
) -> AnchorsReport {
    let mut report = AnchorsReport::default();
    for (idx, line) in anchors_text
        .lines()
        .enumerate()
        .filter(|(_, l)| !l.trim().is_empty())
    {
        report.checked += 1;
        // El lookup se hace tras validar el MAC dentro de verify_anchor_line
        // (el closure de abajo solo corre para líneas con seq parseado; el
        // acceso al mapa es inocuo, el VEREDICTO exige MAC válido primero).
        let seq = serde_json::from_str::<serde_json::Value>(line)
            .ok()
            .and_then(|v| v["seq"].as_i64());
        let at = seq.and_then(|s| hash_by_seq.get(&s).copied());
        match verify_anchor_line(key, line, at) {
            AnchorVerdict::Ok(a) => {
                report.max_ok_seq = Some(report.max_ok_seq.map_or(a.seq, |m| m.max(a.seq)));
            }
            verdict => report.bad.push((idx + 1, verdict)),
        }
    }
    report
}

/// Fila del export: los campos del [`JournalEntry`] en forma estable. Los
/// paths del journal son bytes `to_wire` (regla 1) — normalmente UTF-8 (la
/// forma wire es percent-encoded); si un blob corrupto no lo es, el campo va
/// como `<campo>_hex` y el normal queda `null`, jamás lossy silencioso.
#[derive(Serialize)]
struct AuditRow<'a> {
    seq: i64,
    ts_ms: i64,
    actor_kind: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    actor_id: Option<&'a str>,
    op: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_hex: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_to: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path_to_hex: Option<String>,
    reversal: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    undoes_seq: Option<i64>,
    entry_hash: String,
}

/// Campo de bytes-wire: `(utf8, hex_fallback)`.
fn wire_field(bytes: &[u8]) -> (Option<&str>, Option<String>) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (Some(s), None),
        Err(_) => (None, Some(hex(bytes))),
    }
}

fn row(e: &JournalEntry) -> AuditRow<'_> {
    let (path, path_hex) = wire_field(&e.path);
    let (path_to, path_to_hex) = match &e.path_to {
        None => (None, None),
        Some(b) => wire_field(b),
    };
    AuditRow {
        seq: e.seq,
        ts_ms: e.ts_ms,
        actor_kind: &e.actor_kind,
        actor_id: e.actor_id.as_deref(),
        op: &e.op,
        path,
        path_hex,
        path_to,
        path_to_hex,
        reversal: &e.reversal,
        undoes_seq: e.undoes_seq,
        entry_hash: hex(&e.entry_hash),
    }
}

/// Export JSONL: una línea JSON por entrada, orden de `seq`, claves en orden
/// de declaración (estable entre ejecuciones — apto para diff/firma).
///
/// # Panics
/// Nunca: un struct plano de strings/enteros siempre serializa (el `expect`
/// documenta esa invariante).
#[must_use]
pub fn export_jsonl(entries: &[JournalEntry]) -> String {
    let mut out = String::new();
    for e in entries {
        out.push_str(&serde_json::to_string(&row(e)).expect("struct plano serializa"));
        out.push('\n');
    }
    out
}

/// Escapa un campo CSV (RFC 4180: comillas dobladas; se cita siempre que
/// haga falta) y NEUTRALIZA fórmulas: el CSV es «para humanos» (ADR 0025) —
/// se abrirá en Excel/LibreOffice — y los nombres de archivo son hostiles
/// por diseño (`=HYPERLINK(...)`, `=cmd|...`); un campo que empiece por
/// `=`/`+`/`-`/`@`/TAB se prefija con `'` (convención estándar anti
/// formula-injection en material de auditoría).
fn csv_field(s: &str) -> String {
    let neutralized = if s.starts_with(['=', '+', '-', '@', '\t']) {
        format!("'{s}")
    } else {
        s.to_owned()
    };
    if neutralized.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", neutralized.replace('"', "\"\""))
    } else {
        neutralized
    }
}

/// Export CSV con cabecera fija. Los paths no-UTF-8 van en hex con prefijo
/// `hex:` — AMBIGUO a sabiendas: un path UTF-8 que empiece literalmente por
/// `hex:` es indistinguible (improbable: la forma wire empieza por scheme
/// whitelisted). Para consumo por máquina usa el JSONL, que separa
/// `path`/`path_hex` en campos distintos.
#[must_use]
pub fn export_csv(entries: &[JournalEntry]) -> String {
    let mut out = String::from(
        "seq,ts_ms,actor_kind,actor_id,op,path,path_to,reversal,undoes_seq,entry_hash\n",
    );
    let wire = |b: &[u8]| match std::str::from_utf8(b) {
        Ok(s) => s.to_owned(),
        Err(_) => format!("hex:{}", hex(b)),
    };
    for e in entries {
        let cols = [
            e.seq.to_string(),
            e.ts_ms.to_string(),
            e.actor_kind.clone(),
            e.actor_id.clone().unwrap_or_default(),
            e.op.clone(),
            wire(&e.path),
            e.path_to.as_deref().map(wire).unwrap_or_default(),
            e.reversal.clone(),
            e.undoes_seq.map(|s| s.to_string()).unwrap_or_default(),
            hex(&e.entry_hash),
        ];
        let line: Vec<String> = cols.iter().map(|c| csv_field(c)).collect();
        out.push_str(&line.join(","));
        out.push('\n');
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(seq: i64, path: &[u8]) -> JournalEntry {
        JournalEntry {
            seq,
            ts_ms: 1_700_000_000_000 + seq,
            entry_hash: vec![u8::try_from(seq).unwrap_or(0); 32],
            actor_kind: "agent".into(),
            actor_id: Some("claude".into()),
            op: "created".into(),
            path: path.to_vec(),
            path_to: None,
            reversal: "delete".into(),
            reversal_ref: None,
            undoes_seq: None,
        }
    }

    #[test]
    fn jsonl_es_estable_y_una_linea_por_entrada() {
        let e = [entry(1, b"mem:///a.txt"), entry(2, b"mem:///b.txt")];
        let out = export_jsonl(&e);
        assert_eq!(out.lines().count(), 2);
        let first: serde_json::Value = serde_json::from_str(out.lines().next().unwrap()).unwrap();
        assert_eq!(first["seq"], 1);
        assert_eq!(first["path"], "mem:///a.txt");
        assert_eq!(first["actor_id"], "claude");
        assert_eq!(out, export_jsonl(&e), "determinista");
    }

    #[test]
    fn jsonl_bytes_no_utf8_van_en_hex_jamas_lossy() {
        let e = [entry(1, b"\xff\xfe")];
        let v: serde_json::Value =
            serde_json::from_str(export_jsonl(&e).lines().next().unwrap()).unwrap();
        assert!(v.get("path").is_none(), "sin path lossy");
        assert_eq!(v["path_hex"], "fffe");
    }

    #[test]
    fn csv_escapa_comas_y_comillas() {
        let e = [entry(1, br#"mem:///a,"b".txt"#)];
        let out = export_csv(&e);
        let data = out.lines().nth(1).unwrap();
        assert!(data.contains(r#""mem:///a,""b"".txt""#), "{data}");
    }

    #[test]
    fn ancla_round_trip_y_mac_fabricado_se_rechaza() {
        let key = [7u8; 32];
        let a = Anchor {
            seq: 5,
            head: [9u8; 32],
        };
        let line = anchor_line(&key, &a);
        assert_eq!(
            verify_anchor_line(&key, &line, Some(a.head)),
            AnchorVerdict::Ok(a)
        );
        // Otra clave NO produce la misma línea válida.
        assert_eq!(
            verify_anchor_line(&[8u8; 32], &line, Some(a.head)),
            AnchorVerdict::BadMac
        );
        // Línea fabricada sin la clave: MAC inválido antes de mirar nada.
        let forged = line.replace("\"seq\":5", "\"seq\":6");
        assert_eq!(
            verify_anchor_line(&key, &forged, Some(a.head)),
            AnchorVerdict::BadMac
        );
    }

    #[test]
    fn ancla_detecta_truncacion_y_reescritura() {
        let key = [7u8; 32];
        let a = Anchor {
            seq: 5,
            head: [9u8; 32],
        };
        let line = anchor_line(&key, &a);
        // Truncación/rollback: la cadena ya no llega al seq anclado.
        assert_eq!(
            verify_anchor_line(&key, &line, None),
            AnchorVerdict::MissingSeq(a)
        );
        // Reescritura: el seq existe con OTRO hash.
        assert_eq!(
            verify_anchor_line(&key, &line, Some([1u8; 32])),
            AnchorVerdict::HashMismatch(a)
        );
    }

    #[test]
    fn linea_ilegible_es_bad_line() {
        assert_eq!(
            verify_anchor_line(&[7u8; 32], "no-json", None),
            AnchorVerdict::BadLine
        );
        assert_eq!(
            verify_anchor_line(&[7u8; 32], r#"{"seq":1,"head":"corto","mac":"00"}"#, None),
            AnchorVerdict::BadLine
        );
    }

    /// GOLDEN del formato de línea persistido: clave/seq/head fijos → línea
    /// EXACTA. Si esto cambia, el fichero de anclas existente deja de
    /// verificar: exige bump del context string ("norte-anchor-v2").
    #[test]
    fn golden_formato_de_linea_de_ancla() {
        let line = anchor_line(
            &[7u8; 32],
            &Anchor {
                seq: 5,
                head: [9u8; 32],
            },
        );
        assert_eq!(
            line,
            "{\"seq\":5,\"head\":\"0909090909090909090909090909090909090909090909090909090909090909\",\"mac\":\"b63195ea067b1bf43048bec2b4e3f2707984f8a9680f131cad30b0a44f7a6e1e\"}",
        );
    }

    #[test]
    fn csv_neutraliza_formulas() {
        let mut e = entry(1, b"=HYPERLINK(\"http://evil\")");
        e.actor_id = Some("-2-2".into());
        let out = export_csv(&[e]);
        let data = out.lines().nth(1).unwrap();
        assert!(
            data.contains("'=HYPERLINK"),
            "formula del path neutralizada: {data}"
        );
        assert!(
            data.contains(",'-2-2,"),
            "actor_id elegido por el agente tambien: {data}"
        );
    }

    #[test]
    fn verify_anchors_reporta_cobertura_y_malas() {
        let key = [7u8; 32];
        let a1 = Anchor {
            seq: 1,
            head: [1u8; 32],
        };
        let a2 = Anchor {
            seq: 3,
            head: [3u8; 32],
        };
        let text = format!(
            "{}\n\n{}\nbasura\n",
            anchor_line(&key, &a1),
            anchor_line(&key, &a2)
        );
        let mut chain = std::collections::HashMap::new();
        chain.insert(1, [1u8; 32]);
        chain.insert(3, [3u8; 32]);
        let r = verify_anchors(&key, &text, &chain);
        assert_eq!(r.checked, 3, "las vacias no cuentan");
        assert_eq!(r.max_ok_seq, Some(3), "cobertura = seq mas alto Ok");
        assert_eq!(r.bad, vec![(4, AnchorVerdict::BadLine)]);
        // Recorte del par: la cadena retrocedio a seq<3 -> MissingSeq.
        chain.remove(&3);
        let r = verify_anchors(&key, &text, &chain);
        assert_eq!(r.max_ok_seq, Some(1), "la cobertura RETROCEDE — visible");
        assert!(matches!(
            r.bad.as_slice(),
            [
                (3, AnchorVerdict::MissingSeq(_)),
                (4, AnchorVerdict::BadLine)
            ]
        ));
    }
}
