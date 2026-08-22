//! Los efectos NATIVOS que el host pide: portapapeles, abrir con el
//! escritorio, terminal (tarea 6.5).
//!
//! **Por qué está aquí y no en el host, ni en la webview.** El host dice QUÉ
//! hay que hacer, con operandos que salen de su estado semántico; este
//! proceso decide CÓMO, con una puerta estrecha por cosa. Y la webview no
//! participa: sus capabilities son escuchar eventos y nada más (ADR 0066,
//! decisión D11), así que ni ve las rutas ni tiene con qué ejecutar nada.
//!
//! **Ninguna de las tres es un shell.** Cada una construye un `argv` cerrado
//! —el programa sale de una lista, nunca de un texto del usuario— y no pasa
//! por un intérprete: nada de `sh -c`, que es donde un nombre de fichero con
//! `;` deja de ser un nombre. La elección de programa vive en
//! `norte_frontend::shell`/`openers`, que no hacen I/O y se prueban sin tty;
//! aquí solo se prueba el PATH y se lanza.
//!
//! El texto del portapapeles va por STDIN, nunca en el `argv`: un nombre es
//! bytes, y uno que empiece por `-` sería una bandera para el helper.

use std::process::Stdio;

use norte_ui_host::dto::NativeEffect;

/// Lo que pasó con un efecto. Se DICE: «copiado» sobre un portapapeles vacío
/// solo se descubre cuando el pegado va a otro sitio.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resultado {
    /// Se lanzó (o se escribió) sin error.
    Hecho,
    /// No hay ningún programa en el PATH que sepa hacerlo.
    SinPrograma,
    /// Había programa y falló al arrancar o al escribir.
    Fallo,
}

/// Consume los efectos nativos del host hasta que el canal se cierre.
///
/// Cada uno se ejecuta en un hilo bloqueante: arrancar un proceso y escribir
/// en su stdin son llamadas bloqueantes, y hacerlas en el ejecutor async es
/// la regla 2 rota en un sitio donde nadie lo miraría.
pub async fn bombear(mut rx: tokio::sync::broadcast::Receiver<NativeEffect>) {
    loop {
        match rx.recv().await {
            Ok(efecto) => {
                // Sin esperar al resultado: un `xdg-open` puede tardar
                // segundos en devolver, y el siguiente gesto de quien está
                // delante no espera a que su PDF abra.
                tokio::task::spawn_blocking(move || ejecutar(&efecto));
            }
            // Retrasado: se perdió algún gesto, no un trozo de pantalla. Se
            // sigue escuchando, que es lo contrario de lo que hace la vista.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}

/// Hace UNO. Bloquea: se llama desde `spawn_blocking`.
#[must_use]
pub fn ejecutar(efecto: &NativeEffect) -> Resultado {
    match efecto {
        NativeEffect::CopyBytes { bytes, .. } => copiar(bytes),
        NativeEffect::OpenPath { path } => abrir(path),
        NativeEffect::OpenTerminal { dir } => terminal(dir),
    }
}

/// Escribe `bytes` en el portapapeles con el primer helper que exista.
fn copiar(bytes: &[u8]) -> Resultado {
    use std::io::Write as _;
    for argv in norte_frontend::shell::clipboard_candidates() {
        let Some(programa) = argv.first() else {
            continue;
        };
        let Some(ruta) = programa
            .to_str()
            .and_then(norte_frontend::openers::resolve_program)
        else {
            continue;
        };
        let hijo = std::process::Command::new(ruta)
            .args(&argv[1..])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(mut hijo) = hijo else {
            continue;
        };
        // El texto por STDIN y el stdin CERRADO después: `wl-copy` y `xclip`
        // se quedan de dueños de la selección hasta que el flujo acaba, y sin
        // cerrarlo el portapapeles queda a medias para siempre.
        let escrito = hijo
            .stdin
            .take()
            .map(|mut w| w.write_all(bytes).and_then(|()| w.flush()));
        return match escrito {
            Some(Ok(())) => Resultado::Hecho,
            _ => Resultado::Fallo,
        };
    }
    Resultado::SinPrograma
}

/// Abre `path` con la aplicación que el escritorio elija.
fn abrir(path: &norte_proto::VPath) -> Resultado {
    let Ok(nativa) = norte_vfs_local::vpath_to_native(path) else {
        // El host ya lo comprueba; aquí es el cinturón: a `xdg-open` no se le
        // da algo que no está en este disco.
        return Resultado::SinPrograma;
    };
    let (programa, argv) = norte_frontend::openers::system_opener(&nativa);
    let Some(ruta) = norte_frontend::openers::resolve_program(&programa) else {
        return Resultado::SinPrograma;
    };
    lanzar(&ruta, &argv[1..], None)
}

/// Abre un terminal sentado en `dir`.
fn terminal(dir: &norte_proto::VPath) -> Resultado {
    let Ok(nativa) = norte_vfs_local::vpath_to_native(dir) else {
        return Resultado::SinPrograma;
    };
    for argv in norte_frontend::shell::terminal_candidates(&nativa) {
        let Some(programa) = argv.first().and_then(|p| p.to_str()) else {
            continue;
        };
        let Some(ruta) = norte_frontend::openers::resolve_program(programa) else {
            continue;
        };
        // Y con el cwd puesto ADEMÁS de la bandera: `xterm` no tiene bandera
        // y hereda el directorio, que es justo el caso que la lista
        // compartida documenta.
        return lanzar(&ruta, &argv[1..], Some(&nativa));
    }
    Resultado::SinPrograma
}

/// Lanza y SUELTA: la ventana no espera a que un PDF abra.
fn lanzar(
    programa: &std::path::Path,
    args: &[std::ffi::OsString],
    cwd: Option<&std::path::Path>,
) -> Resultado {
    let mut cmd = std::process::Command::new(programa);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(d) = cwd {
        cmd.current_dir(d);
    }
    match cmd.spawn() {
        Ok(_) => Resultado::Hecho,
        Err(_) => Resultado::Fallo,
    }
}

#[cfg(test)]
mod tests {
    use super::{Resultado, ejecutar};
    use norte_ui_host::dto::NativeEffect;

    /// Una localización que NO está en este disco no se le da al escritorio.
    ///
    /// El host ya lo comprueba y lo dice; esto es el cinturón del otro lado:
    /// a `xdg-open` no se le pasa un `sftp://`, y a un terminal no se le da
    /// un directorio donde no puede sentarse. Sin esta guarda, la conversión
    /// fallaría en silencio y el usuario vería «abriendo…» sobre nada.
    #[test]
    fn lo_que_no_esta_en_este_disco_no_se_lanza() {
        let remoto = norte_proto::VPath::parse("sftp://maquina/casa/x").expect("vpath");
        assert_eq!(
            ejecutar(&NativeEffect::OpenPath {
                path: remoto.clone()
            }),
            Resultado::SinPrograma
        );
        assert_eq!(
            ejecutar(&NativeEffect::OpenTerminal { dir: remoto }),
            Resultado::SinPrograma
        );
    }

    /// El portapapeles se intenta con los helpers del sistema, y cuando no
    /// hay ninguno se DICE en vez de decir que copió.
    ///
    /// No se afirma cuál gana: en la máquina de CI puede no haber ninguno, y
    /// en la de un humano puede haber dos. Lo que se pina es que el resultado
    /// es uno de los tres y jamás un pánico con bytes que no son UTF-8.
    #[test]
    fn copiar_bytes_no_decodifica_ni_revienta() {
        let r = ejecutar(&NativeEffect::CopyBytes {
            // Un nombre que no es UTF-8: por STDIN va tal cual, y el
            // portapapeles recibe los MISMOS bytes que abren ese fichero.
            bytes: vec![b'/', b't', b'm', b'p', b'/', 0xFF, 0xFE],
            count: 1,
        });
        assert!(matches!(
            r,
            Resultado::Hecho | Resultado::SinPrograma | Resultado::Fallo
        ));
    }
}
