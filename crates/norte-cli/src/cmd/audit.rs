//! `norte audit` (M3-5, ADR 0025): cadena + anclas + export.

use std::process::ExitCode;

use anyhow::Context;

use crate::cmd::entorno::append_line_0600;
use crate::{AuditCmd, AuditFormat};

/// `norte audit <verify|export|anchor>` (M3-5, ADR 0025): opera sobre la DB
/// del journal en SOLO-LECTURA. Con el daemon corriendo, `SQLite` devuelve
/// `database is locked` (su lock es exclusivo): el mensaje lo dice claro.
///
/// Desde #167 el daemon no es el único que puede tenerlo: un frontend embebido
/// —un `ntc` sin `--daemon`— se queda el mismo lock. Pero solo DESDE QUE MUTA
/// algo (#177): un `ntc` navegando no estorba a este comando, y por eso el
/// texto de ayuda no manda cerrar los frontends, solo dice quién puede tenerlo.
pub(crate) async fn audit_cmd(cmd: AuditCmd) -> anyhow::Result<ExitCode> {
    use norte_core::{Journal, audit};
    let dir = norte_core::connect::config_dir();
    let journal_path = dir.join("journal.db");
    let anchors_path = dir.join("journal-anchors.jsonl");
    // Las anclas del MARCADOR viven en su PROPIO fichero (#146), y no como una
    // línea más de las del head. El motivo es de compatibilidad y es del tipo
    // que se paga caro: `verify_anchors` busca cada `seq` anclado en el mapa
    // que le pasa el llamante, y un binario ANTERIOR a este cambio no siembra
    // el `seq` 0 — así que leería la línea del marcador como `MissingSeq`, o
    // sea «el seq anclado ya no existe: truncación o rollback». Una acusación
    // FALSA de manipulación contra un fichero que nadie tocó, emitida por el
    // arreglo del ADR cuya razón de ser es no emitir exactamente eso. Y no se
    // arregla con otra forma de línea: ese verificador falla en cerrado ante
    // todo lo que no entiende, así que un JSON distinto saldría por `BadLine`.
    //
    // Con dos ficheros, un binario viejo simplemente no lo abre: no gana la
    // cobertura nueva —que tampoco tenía— y no pierde nada.
    let marker_anchors_path = dir.join("journal-marker-anchors.jsonl");
    let journal = Journal::open_read_only(&journal_path)
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))
        .context(norte_i18n::t("cli-audit-open-failed"))?;
    match cmd {
        AuditCmd::Export { format } => {
            let entries = journal
                .entries()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
                .context(norte_i18n::t("cli-audit-open-failed"))?;
            let out = match format {
                AuditFormat::Jsonl => audit::export_jsonl(&entries),
                AuditFormat::Csv => audit::export_csv(&entries),
            };
            print!("{out}");
            Ok(ExitCode::SUCCESS)
        }
        AuditCmd::Anchor => {
            // Jamás se ancla una cadena que este binario no haya podido
            // verificar: el ancla fijaría como «buena» una historia rota, o una
            // que no sabe leer (ADR 0046).
            let status = journal
                .verify_chain()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if !status.is_intact() {
                let declared = journal.format().await.map_err(|e| anyhow::anyhow!("{e}"))?;
                report_chain_not_certified(&status, declared);
                return Ok(ExitCode::FAILURE);
            }
            let cabeza = journal.head().await.map_err(|e| anyhow::anyhow!("{e}"))?;
            // El MARCADOR DE FORMATO (`seq 0`) se ancla también, y primero
            // (#146). ADR 0046 concedía que re-declararlo cuesta tres
            // escrituras de columna y ninguna clave, y que las anclas del HEAD
            // no lo cazan porque esa edición no mueve ningún `entry_hash` de
            // `seq >= 1`. Firmarlo aparte convierte esa re-declaración en un
            // `HashMismatch` en el `seq` 0: localizada, y con una clave detrás.
            //
            // Va antes que la del head para que un journal que solo tiene
            // marcador —recién creado, sin una sola mutación— quede cubierto
            // igual; ahí `head()` es `None` y abajo se sale.
            let marcador = journal
                .marker_hash()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            if marcador.is_none() && cabeza.is_none() {
                println!("{}", norte_i18n::t("cli-audit-empty"));
                return Ok(ExitCode::SUCCESS);
            }
            // La clave del keyring puede bloquear (D-Bus/prompt): fuera del
            // reactor (regla 2).
            let key = tokio::task::spawn_blocking(norte_core::connect::journal_anchor_key)
                .await
                .map_err(|_| anyhow::anyhow!("keyring task panicked"))?
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // Las líneas salen por stdout A PROPÓSITO: la copia EXTERNA de las
            // anclas (log remoto, otro host) es lo que hace detectable el
            // recorte del fichero local (ADR 0025).
            if let Some(head) = marcador {
                let line = audit::anchor_line(&key, &audit::Anchor { seq: 0, head });
                // El ancla del marcador es DETERMINISTA: el marcador no cambia
                // nunca, así que anclar diez veces escribiría diez líneas
                // idénticas y el informe contaría diez anclas verificadas donde
                // hay una. Se escribe solo si no está ya.
                if !ya_anclado(&marker_anchors_path, &line).await? {
                    append_line_0600(&marker_anchors_path, &line).await?;
                }
                println!("{line}");
                println!("{}", norte_i18n::t("cli-audit-anchored-marker"));
            }
            let Some((seq, head)) = cabeza else {
                // Y no «journal vacío: nada que anclar», que contradiría en la
                // misma pantalla a la línea de arriba.
                println!("{}", norte_i18n::t("cli-audit-only-marker"));
                return Ok(ExitCode::SUCCESS);
            };
            let line = audit::anchor_line(&key, &audit::Anchor { seq, head });
            append_line_0600(&anchors_path, &line).await?;
            println!("{line}");
            println!(
                "{}",
                norte_i18n::ta("cli-audit-anchored", &[("seq", &seq.to_string())])
            );
            Ok(ExitCode::SUCCESS)
        }
        AuditCmd::Verify { allow_no_anchors } => {
            audit_verify(
                &journal,
                &anchors_path,
                &marker_anchors_path,
                allow_no_anchors,
            )
            .await
        }
    }
}

/// Por qué la cadena NO quedó certificada, en la voz que corresponde: una
/// rotura es una acusación y se cita dónde; un formato desconocido (ADR 0046)
/// NO lo es —este binario no sabe recomputar lo que escribió uno más nuevo— y
/// se dice sin acusar a nadie, pero también sin absolver: en los dos casos el
/// audit sale con FALLO.
///
/// Va por STDOUT, igual que `cli-audit-chain-ok`: el veredicto es la SALIDA del
/// audit, no un diagnóstico suelto, y un `norte audit verify > informe.txt` que
/// guarde la cobertura y las anclas pero no el veredicto es justo el fichero
/// que no hay que producir. El fallo lo lleva el código de salida.
///
/// `declared` viene del journal porque el veredicto `Broken` no lo lleva: una
/// cadena rota EN un journal que además está escrito en un formato ilegible es
/// una rotura que hay que leer con esa luz.
fn report_chain_not_certified(
    status: &norte_core::ChainStatus,
    declared: norte_core::JournalFormat,
) {
    use norte_core::{ChainStatus, JournalFormat};
    // `Unmarked` no llega por la rama de formato desconocido (un journal sin
    // marcador se verifica con las reglas de hoy), y cualquier variante futura
    // es, por definición, algo que este binario no sabe leer.
    let name = |f: JournalFormat| match f {
        JournalFormat::Version(v) => v.to_string(),
        _ => norte_i18n::t("cli-audit-format-unreadable"),
    };
    let unknown_format = |declared: JournalFormat, known: u32| {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-chain-unknown-format",
                &[("declared", &name(declared)), ("known", &known.to_string())],
            )
        );
    };
    match status {
        ChainStatus::Broken { first_bad_seq } => {
            if declared.is_unknown() {
                unknown_format(declared, norte_core::JOURNAL_FORMAT);
            }
            println!(
                "{}",
                norte_i18n::ta(
                    "cli-audit-chain-broken",
                    &[("seq", &first_bad_seq.to_string())],
                )
            );
        }
        ChainStatus::UnknownFormat {
            declared,
            known,
            first_unverifiable_seq,
        } => {
            unknown_format(*declared, *known);
            if let Some(seq) = first_unverifiable_seq {
                println!(
                    "{}",
                    norte_i18n::ta(
                        "cli-audit-chain-unverifiable-from",
                        &[("seq", &seq.to_string())],
                    )
                );
            }
        }
        // Un veredicto que este binario no conoce se trata como NO certificado.
        // `ChainStatus` es `#[non_exhaustive]` justamente para que un veredicto
        // nuevo llegue aquí en vez de colarse por la rama de «íntegra».
        _ => println!("{}", norte_i18n::t("cli-audit-chain-not-certified")),
    }
}

/// `norte audit verify`: cadena (cita la primera rotura, B2) + anclas +
/// COBERTURA (hasta qué seq llegan las anclas Ok — el recorte del fichero de
/// anclas se manifiesta como cobertura que retrocede). Sin anclas = FALLO
/// salvo `--allow-no-anchors`: la ausencia es indistinguible de un borrado
/// hostil (H1 del security-reviewer).
async fn audit_verify(
    journal: &norte_core::Journal,
    anchors_path: &std::path::Path,
    marker_anchors_path: &std::path::Path,
    allow_no_anchors: bool,
) -> anyhow::Result<ExitCode> {
    use norte_core::{ChainStatus, audit};
    let status = journal
        .verify_chain()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // Una cadena ROTA ya trae su culpable y su sitio: no hay segunda opinión
    // que buscar. Una que este binario NO SABE LEER (ADR 0046) es al revés —
    // las anclas son la única evidencia que discrimina «journal más nuevo» de
    // «marcador re-declarado», no necesitan recomputar la cadena (contrastan
    // hashes ALMACENADOS) y el operador ya las tiene en disco. Así que se sigue
    // hasta el informe de anclas y se sale con FALLO igual.
    let certified = match status {
        ChainStatus::Intact { entries } => {
            println!(
                "{}",
                norte_i18n::ta("cli-audit-chain-ok", &[("entries", &entries.to_string())])
            );
            true
        }
        ChainStatus::UnknownFormat { .. } => {
            report_chain_not_certified(&status, declared_format(journal).await?);
            false
        }
        _ => {
            report_chain_not_certified(&status, declared_format(journal).await?);
            return Ok(ExitCode::FAILURE);
        }
    };
    let head_seq = journal
        .head()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .map(|(seq, _)| seq);
    // La clave ANTES de decidir nada sobre las anclas del head: el marcador se
    // contrasta pase lo que pase con ellas, y sin clave no se puede contrastar
    // ninguna de las dos familias.
    let key = tokio::task::spawn_blocking(norte_core::connect::journal_anchor_key)
        .await
        .map_err(|_| anyhow::anyhow!("keyring task panicked"))?
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    let lines = match tokio::fs::read_to_string(&anchors_path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // El MARCADOR se comprueba igual, y esta es la razón de que su
            // comprobación viva antes de esta salida: un journal con marcador y
            // sin mutaciones tiene ancla de marcador y NINGUNA de head, y sin
            // esto `anchor` y `verify` se contradecían dentro del mismo commit
            // —una escribía el ancla y la otra jamás la miraba—. Es además el
            // estado que deja un atacante que borra el fichero de anclas del
            // head: la señal del marcador es lo único que queda.
            let marcador_ok = verify_marker_anchors(journal, marker_anchors_path, &key).await?;
            let msg = norte_i18n::t("cli-audit-no-anchors");
            if allow_no_anchors {
                println!("{msg}");
                // `--allow-no-anchors` perdona la AUSENCIA de anclas de head, no
                // una cadena sin certificar ni un marcador sin anclar.
                return Ok(if marcador_ok {
                    exit_for(certified)
                } else {
                    ExitCode::FAILURE
                });
            }
            // Ausencia = fallo por defecto: un atacante sin clave puede
            // BORRAR el fichero; solo el humano decide que «no hay» es ok.
            eprintln!("{msg}");
            return Ok(ExitCode::FAILURE);
        }
        Err(e) => return Err(e).context("journal-anchors.jsonl"),
    };
    // UN snapshot de la cadena para todo el veredicto (sin TOCTOU entre el
    // verify de arriba y los contrastes de anclas).
    let hash_by_seq: std::collections::HashMap<i64, [u8; 32]> = journal
        .entries()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .into_iter()
        .filter_map(|e| e.entry_hash.try_into().ok().map(|h: [u8; 32]| (e.seq, h)))
        .collect();

    let report = audit::verify_anchors(&key, &lines, &hash_by_seq);
    report_anchors(&report, head_seq, certified);
    let marcador_ok = verify_marker_anchors(journal, marker_anchors_path, &key).await?;
    if !report.bad.is_empty() || !marcador_ok {
        return Ok(ExitCode::FAILURE);
    }
    if certified {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchors-ok",
                &[("count", &report.checked.to_string())],
            )
        );
    }
    Ok(exit_for(certified))
}

/// Contrasta las anclas del MARCADOR (#146) y dice si el marcador se quedó SIN
/// anclar. `false` = hay algo que reprochar y el comando sale con fallo.
///
/// # Por qué el «sin anclar» es una línea propia y no un silencio
/// La defensa de ADR 0025 contra el recorte del fichero de anclas es que la
/// COBERTURA retrocede, y eso solo funciona para la COLA. El ancla del marcador
/// es la de `seq` más bajo que existe, así que borrarla —o borrar su fichero
/// entero— no mueve `max_ok_seq` ni un dígito: el informe no diría nada. La
/// receta del atacante pasaría de tres escrituras a cuatro sobre ficheros que
/// ya puede escribir.
///
/// La misma línea cubre el otro hueco, y este no se puede cerrar de ninguna
/// otra forma: un journal SIN marcador (todos los que existían antes de ADR
/// 0046, que por diseño no lo ganan nunca) admite que le INYECTEN uno —
/// insertar la fila del `seq` 0 y reencadenar el `seq` 1— y eso convierte un
/// `Broken` localizado en `UnknownFormat` igual que la re-declaración. Ahí no
/// hay ancla previa que contradecir, porque cuando se ancló no había marcador.
/// Lo que sí se puede decir es que AHORA hay un marcador y nadie lo ha
/// anclado, que es exactamente lo que un marcador inyectado produce.
///
/// # Errors
/// Lectura del fichero de anclas del marcador, o del journal.
async fn verify_marker_anchors(
    journal: &norte_core::Journal,
    path: &std::path::Path,
    key: &[u8],
) -> anyhow::Result<bool> {
    use norte_core::audit;
    let Some(marcador) = journal
        .marker_hash()
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?
    else {
        // Sin marcador no hay nada que anclar ni nada que reprochar. Un fichero
        // de anclas de marcador SOBRE un journal sin marcador sí sería raro,
        // pero es el caso de abajo (`MissingSeq`) y se cuenta como malo.
        return Ok(true);
    };
    let lines = match tokio::fs::read_to_string(path).await {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e).context("journal-marker-anchors.jsonl"),
    };
    let snapshot: std::collections::HashMap<i64, [u8; 32]> =
        std::iter::once((0, marcador)).collect();
    let report = audit::verify_anchors(key, &lines, &snapshot);
    for (line_no, verdict) in &report.bad {
        let Some(detail) = verdict_detail(verdict) else {
            continue;
        };
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchor-bad",
                &[("line", &line_no.to_string()), ("detail", &detail)],
            )
        );
    }
    if report.max_ok_seq.is_none() {
        eprintln!("{}", norte_i18n::t("cli-audit-marker-unanchored"));
        return Ok(false);
    }
    println!("{}", norte_i18n::t("cli-audit-marker-ok"));
    Ok(report.bad.is_empty())
}

/// ¿Está ya esa línea EXACTA en el fichero? Evita duplicar un ancla que es
/// determinista (la del marcador, que no cambia nunca).
///
/// # Errors
/// Lectura del fichero, salvo su ausencia.
async fn ya_anclado(path: &std::path::Path, line: &str) -> anyhow::Result<bool> {
    match tokio::fs::read_to_string(path).await {
        Ok(s) => Ok(s.lines().any(|l| l == line)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).context("journal-marker-anchors.jsonl"),
    }
}

/// El informe de anclas: las malas una por una, la salvedad cuando la cadena
/// NO quedó certificada, y la cobertura.
///
/// El orden importa. «Ancladas contra la cadena» presupone una cadena
/// verificada; si este binario no pudo verificarla, lo que las anclas dicen es
/// OTRA frase —los hashes almacenados no se han movido desde que se ancló— y
/// esa salvedad va ANTES de la cobertura, porque «hasta el seq 100 de 100»
/// leída sin ella es la línea que el operador citará como visto bueno.
fn report_anchors(
    report: &norte_core::audit::AnchorsReport,
    head_seq: Option<i64>,
    certified: bool,
) {
    for (line_no, verdict) in &report.bad {
        let Some(detail) = verdict_detail(verdict) else {
            continue;
        };
        eprintln!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchor-bad",
                &[("line", &line_no.to_string()), ("detail", &detail)],
            )
        );
    }
    if !certified {
        println!(
            "{}",
            norte_i18n::ta(
                "cli-audit-anchors-ok-unverified-chain",
                &[("count", &report.checked.to_string())],
            )
        );
    }
    // Cobertura SIEMPRE visible: anclas hasta X, cadena hasta Y. Un recorte
    // del fichero de anclas retrocede X sin tocar la cadena.
    println!(
        "{}",
        norte_i18n::ta(
            "cli-audit-coverage",
            &[
                (
                    "anchored",
                    &report
                        .max_ok_seq
                        .map_or_else(|| "-".into(), |s| s.to_string()),
                ),
                (
                    "head",
                    &head_seq.map_or_else(|| "-".into(), |s| s.to_string()),
                ),
            ],
        )
    );
}

/// El formato que DECLARA el journal, para poner el veredicto en contexto.
async fn declared_format(
    journal: &norte_core::Journal,
) -> anyhow::Result<norte_core::JournalFormat> {
    journal.format().await.map_err(|e| anyhow::anyhow!("{e}"))
}

/// Éxito solo si la cadena quedó certificada.
fn exit_for(certified: bool) -> ExitCode {
    if certified {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

/// Traduce un veredicto NO-Ok de ancla a su mensaje Fluent.
fn verdict_detail(verdict: &norte_core::audit::AnchorVerdict) -> Option<String> {
    use norte_core::audit::AnchorVerdict;
    Some(match verdict {
        AnchorVerdict::BadLine => norte_i18n::t("cli-audit-verdict-bad-line"),
        AnchorVerdict::BadMac => norte_i18n::t("cli-audit-verdict-bad-mac"),
        AnchorVerdict::MissingSeq(a) => {
            norte_i18n::ta("cli-audit-verdict-missing", &[("seq", &a.seq.to_string())])
        }
        AnchorVerdict::HashMismatch(a) => {
            norte_i18n::ta("cli-audit-verdict-mismatch", &[("seq", &a.seq.to_string())])
        }
        AnchorVerdict::Ok(_) => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ADR 0046: every verdict that is not `Intact` has something to say in
    /// both locales, and the "unknown format" one says the version WITHOUT
    /// accusing anyone. A missing Fluent key falls back to the key itself,
    /// which in this path would be the whole message the operator gets.
    #[test]
    fn cada_veredicto_no_certificado_tiene_su_mensaje_en_los_dos_idiomas() {
        use norte_core::{ChainStatus, JournalFormat};
        for lang in [norte_i18n::Lang::En, norte_i18n::Lang::Es] {
            let unknown = norte_i18n::ta_in(
                lang,
                "cli-audit-chain-unknown-format",
                &[("declared", "999"), ("known", "1")],
            );
            assert!(unknown.contains("999"), "{lang:?}: {unknown}");
            assert!(!unknown.starts_with("cli-audit"), "{lang:?}: sin traducir");
            for key in [
                "cli-audit-chain-unverifiable-from",
                "cli-audit-chain-not-certified",
                "cli-audit-format-unreadable",
            ] {
                let msg = norte_i18n::ta_in(lang, key, &[("seq", "7")]);
                assert!(
                    !msg.starts_with("cli-audit"),
                    "{lang:?}/{key}: sin traducir"
                );
            }
        }
        // Y no panica con ninguna forma del veredicto (incluida la rama de
        // cierre en falso, que es lo que verá un veredicto futuro).
        for status in [
            ChainStatus::Broken { first_bad_seq: 3 },
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Version(999),
                known: norte_core::JOURNAL_FORMAT,
                first_unverifiable_seq: Some(1),
            },
            ChainStatus::UnknownFormat {
                declared: JournalFormat::Unreadable,
                known: norte_core::JOURNAL_FORMAT,
                first_unverifiable_seq: None,
            },
        ] {
            report_chain_not_certified(&status, JournalFormat::Version(999));
        }
        // Y una rotura en un journal cuyo formato tampoco se puede leer dice
        // las DOS cosas: la rotura es verdad, y sin la salvedad no se puede
        // interpretar.
        report_chain_not_certified(
            &ChainStatus::Broken { first_bad_seq: 3 },
            JournalFormat::Unreadable,
        );
    }
}
