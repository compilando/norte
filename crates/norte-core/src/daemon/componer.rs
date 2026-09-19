//! Componer el daemon de `norte daemon run`: qué piezas lleva y en qué orden.
//!
//! Vivía en la CLI (regla 7): el journal, el barrido de spools, la policy, el
//! índice, la IA, los límites de archivo y el `bind` son decisiones del core,
//! y el binario que las hacía era el único sitio donde un segundo arranque —un
//! test, otro frontend, un embebedor— no las podía repetir sin copiarlas.
//!
//! Aquí se COMPONE y no se habla: lo que el operador tiene que saber vuelve
//! como [`Aviso`] tipado y lo pinta quien lanza, en su idioma. Lo que impide
//! arrancar vuelve como [`ErrorDeArranque`].

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use super::{Daemon, DaemonApprovalResolver, DaemonConfig, DaemonError};
use crate::Engine;

/// Lo que decide quien lanza el daemon.
#[derive(Debug, Clone, Default)]
pub struct Opciones {
    /// El socket; `None` = el del sistema.
    pub socket: Option<PathBuf>,
    /// Cuánto sin clientes antes de salir; `None` = nunca.
    pub idle_timeout: Option<Duration>,
    /// Dónde persiste la sesión de UI; `None` = no se persiste.
    pub state_dir: Option<PathBuf>,
    /// De dónde salen el journal, el índice, los spools, las conexiones y los
    /// secretos de la IA; `None` = el del usuario (`connect::config_dir`).
    ///
    /// NO cubre la configuración: `policy.toml`, `[ai]` y `[archive]` se
    /// leen de las capas estándar del usuario. Un test que pase del journal
    /// leería la config real, así que los de este módulo paran antes.
    pub config_dir: Option<PathBuf>,
}

/// Algo que el operador debe saber y que NO impide arrancar. El mismo tipo
/// que devuelve [`crate::equipo::equipar`], que es de donde salen los de IA.
pub use crate::equipo::Aviso;

/// Lo que impide arrancar.
///
/// El mensaje NO repite la causa: va en `source()`, y quien imprime la
/// cadena (`{e:#}` de anyhow) ya la añade. Con `{0}` en el mensaje salía dos
/// veces.
#[derive(Debug, thiserror::Error)]
pub enum ErrorDeArranque {
    /// El journal no abre; con «database is locked», otro proceso lo tiene.
    #[error("no se pudo abrir el journal")]
    Journal(#[source] crate::journal::JournalError),
    /// `policy.toml` no se leyó o no es válido.
    #[error("policy.toml")]
    Policy(#[source] std::io::Error),
    /// `[archive]` de `norte.toml` no se leyó o no es válido.
    #[error("norte.toml ([archive])")]
    Archivo(#[source] std::io::Error),
    /// El socket no se pudo enlazar.
    #[error("no se pudo enlazar el daemon")]
    Bind(#[source] DaemonError),
}

/// Compone el daemon con todo lo suyo, listo para `run`.
///
/// El orden importa y está escrito en cada paso: el journal PRIMERO, porque su
/// lock exclusivo es lo que garantiza que no hay otro daemon sobre este
/// directorio de estado; el barrido de spools DESPUÉS, por eso mismo.
///
/// Los avisos van a `avisos` A MEDIDA que ocurren, y no en el `Ok`: si el
/// arranque falla después —un `[archive]` roto, el socket ocupado—, lo que ya
/// se había averiguado por el camino (un spool que no se dejó barrer, un índice
/// que no abrió) sigue siendo verdad y quien lanza lo tiene que poder decir.
///
/// # Errors
/// [`ErrorDeArranque`]: el journal, la policy, los límites de archivo o el
/// `bind`. Todo lo demás degrada con su [`Aviso`].
pub async fn componer(o: Opciones, avisos: &mut Vec<Aviso>) -> Result<Daemon, ErrorDeArranque> {
    let dir = o.config_dir.unwrap_or_else(crate::connect::config_dir);
    // El daemon es el dueño ÚNICO del journal (spec §4, ADR 0024) y quien
    // instala policy + approvals: los agentes MCP se gobiernan aquí, jamás en
    // el puente.
    let journal = crate::SqliteJournal::open(&dir.join("journal.db"))
        .await
        .map_err(ErrorDeArranque::Journal)?;
    // Barrido de spools de sincronización (ADR 0049), junto al journal y por
    // lo mismo: un cierre violento deja ficheros que AUTORIZAN escrituras, y
    // nadie más los va a recoger. Se llevan todos: aquí no hay ninguna conexión
    // viva todavía. Un barrido que falla NO impide arrancar —lo que impide
    // aplicar un plan viejo es que el registro de planes nace vacío—.
    //
    // El `Spool` se construye UNA vez: dos `Spool::new` son dos registros de
    // emisión que no se ven, y este es el que se instala en el engine.
    let spool = crate::sync::Spool::new(dir.clone());
    match spool.sweep().await {
        Ok(r) if r.removed == 0 && r.is_clean() => {}
        Ok(r) if r.is_clean() => avisos.push(Aviso::SpoolsBarridos(r.removed)),
        Ok(r) => avisos.push(Aviso::SpoolsAMedias {
            removed: r.removed,
            failed: r.failed,
            dir: spool.dir().to_path_buf(),
        }),
        Err(e) => avisos.push(Aviso::SpoolsSinBarrer {
            dir: spool.dir().to_path_buf(),
            error: e.to_string(),
        }),
    }
    // policy.toml: ausente = sin reglas = un agente DENTRO de scope aún
    // deniega (fail-closed, `no-rule`).
    let cfg = crate::blocking::spawn_blocking(crate::PolicyConfig::load)
        .await
        .map_err(|e| ErrorDeArranque::Policy(std::io::Error::other(e)))?
        .map_err(ErrorDeArranque::Policy)?;
    let scopes = crate::ScopeRegistry::new();
    let approvals = Arc::new(DaemonApprovalResolver::default());
    let engine = Engine::with_journal(Arc::new(journal)).with_policy(
        Arc::new(crate::ScopedPolicy::new(scopes.clone(), cfg)),
        Arc::clone(&approvals) as _,
    );
    // Índice de búsqueda (M4, ADR 0034). Si no abre, se sigue sin él.
    let engine = crate::equipo::con_indice(engine, &dir, avisos).await;
    engine.set_spool(spool);
    // `[archive]` roto ABORTA el daemon (fail-loud, como `policy.toml`); un
    // frontend embebido, en cambio, lo degrada a aviso.
    crate::archive_config::aplicar(&engine)
        .await
        .map_err(ErrorDeArranque::Archivo)?;
    // Proveedor local, conector e IA (ADR 0031: opt-in, jamás aborta): lo que
    // lleva todo engine, igual que los embebidos.
    avisos.extend(
        crate::equipo::equipar(&engine, &dir, crate::equipo::Ia::TODA)
            .await
            .avisos,
    );
    let daemon = Daemon::bind_with_policy(
        Arc::new(engine),
        scopes,
        approvals,
        DaemonConfig {
            socket_path: o.socket,
            idle_timeout: o.idle_timeout,
            // Se pasa EXPLÍCITO: el default no persiste nada, para que ningún
            // test ni embebedor escriba el estado real por descuido.
            state_dir: o.state_dir,
            ..DaemonConfig::default()
        },
    )
    .await
    .map_err(ErrorDeArranque::Bind)?;
    Ok(daemon)
}

#[cfg(test)]
mod tests {
    use super::{ErrorDeArranque, Opciones, componer};

    /// El journal va PRIMERO y su lock es exclusivo: con otro dueño vivo, el
    /// daemon no arranca, y lo dice con su clase de error. Es lo único que
    /// impide dos daemons sobre un mismo directorio.
    ///
    /// Es también el único camino de `componer` que se puede probar sin leer
    /// la config REAL del usuario: los pasos siguientes cargan `policy.toml`
    /// y `[ai]` por las capas estándar.
    ///
    /// Tarda lo que el `busy_timeout` del journal (5 s): `SQLite` reintenta el
    /// lock antes de rendirse. Se acepta en vez de añadir una opción que solo
    /// usaría este test — es el mismo plazo que ve el operador.
    #[tokio::test]
    async fn con_el_journal_tomado_no_arranca() {
        let dir = tempfile::tempdir().expect("tempdir");
        let _dueno = crate::SqliteJournal::open(&dir.path().join("journal.db"))
            .await
            .expect("primer dueño");
        let mut avisos = Vec::new();
        let r = componer(
            Opciones {
                config_dir: Some(dir.path().to_path_buf()),
                socket: Some(dir.path().join("d.sock")),
                ..Opciones::default()
            },
            &mut avisos,
        )
        .await;
        assert!(
            matches!(r, Err(ErrorDeArranque::Journal(_))),
            "esperaba Journal: {:?}",
            r.err()
        );
        assert!(avisos.is_empty(), "nada se hizo antes del journal");
    }
}
