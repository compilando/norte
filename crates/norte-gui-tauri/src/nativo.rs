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
pub async fn bombear(
    mut rx: tokio::sync::broadcast::Receiver<NativeEffect>,
    host: std::sync::Arc<norte_ui_host::UiHost>,
    tema: impl Fn(&str) + Send + 'static,
) {
    loop {
        match rx.recv().await {
            // El TEMA no se «ejecuta»: se vuelve a resolver aquí, porque los
            // colores cruzan a la webview convertidos en variables CSS y esa
            // conversión es de este proceso. Va sin `spawn_blocking` a
            // propósito: resolver un preset es aritmética sobre colores, no
            // I/O, y mandarlo a otro hilo solo añadiría un frame de retraso a
            // algo que el lector está viendo cambiar bajo el cursor.
            Ok(NativeEffect::ThemeChanged { name }) => tema(&name),
            // El selector de carpeta es el único que CONTESTA (#284): los
            // demás se lanzan y se olvidan, pero de este el host espera una
            // ruta, así que su respuesta vuelve por `dispatch` como cualquier
            // otra acción — la misma puerta que usa el renderer.
            Ok(NativeEffect::PickDirectory { desde }) => {
                let host = std::sync::Arc::clone(&host);
                tokio::task::spawn(async move {
                    let elegido = tokio::task::spawn_blocking(move || elegir_directorio(&desde))
                        .await
                        .unwrap_or(None);
                    let _ = host
                        .dispatch(norte_ui_host::UiAction::DirectoryPicked { path: elegido })
                        .await;
                });
            }
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
        NativeEffect::Notify { titulo, cuerpo } => avisar(titulo, cuerpo),
        // Los dos los atiende `bombear`, y ninguno lanza un programa: al
        // selector de carpeta hay que CONTESTARLE con la ruta, y el tema es un
        // catálogo que rehacer. Aquí no hay nada que ejecutar.
        NativeEffect::PickDirectory { .. } | NativeEffect::ThemeChanged { .. } => {
            Resultado::SinPrograma
        }
    }
}

/// Saca el aviso con el primer programa que exista (#285).
///
/// El texto llega YA compuesto, traducido, enmascarado y acotado: aquí no se
/// decide nada sobre él, solo se entrega. Un aviso que no se puede dar se DICE
/// —`SinPrograma`— en vez de tragarse: quien cree que le van a avisar y no
/// tiene `notify-send` merece saberlo una vez.
fn avisar(titulo: &str, cuerpo: &str) -> Resultado {
    for argv in norte_frontend::shell::notify_candidates(titulo, cuerpo) {
        let Some((programa, args)) = argv.split_first() else {
            continue;
        };
        let salida = std::process::Command::new(programa)
            .args(args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
        match salida {
            Ok(st) if st.success() => return Resultado::Hecho,
            // Está y falló: no se prueba el siguiente. Dos avisos del mismo
            // suceso es peor que ninguno.
            Ok(_) => return Resultado::Fallo,
            // No está en el PATH: al siguiente candidato.
            Err(_) => {}
        }
    }
    Resultado::SinPrograma
}

/// Abre el selector de carpeta del ESCRITORIO y devuelve lo que se eligió
/// (#284). `None` = se cerró sin elegir, o aquí no hay ningún selector.
///
/// Bloquea a propósito —se llama desde `spawn_blocking`—: un selector se queda
/// abierto todo el tiempo que el lector tarde en decidir, que puede ser un
/// minuto.
///
/// Los candidatos se prueban en orden y el primero que EXISTE decide: un
/// programa que no está da `NotFound` al arrancar y se pasa al siguiente, que
/// es el mismo sondeo por intento que hacen el portapapeles y el terminal.
/// Cancelar se distingue de elegir por el código de salida, no analizando el
/// texto — un directorio puede llamarse como cualquier mensaje de error.
#[must_use]
fn elegir_directorio(desde: &norte_proto::VPath) -> Option<String> {
    // Con un panel REMOTO no hay ruta nativa donde abrir, y eso no impide
    // nada: el selector devuelve siempre una carpeta de esta máquina, y copiar
    // de un `sftp://` a una carpeta local es legítimo. Se pierde la sugerencia
    // de dónde empezar, no la operación.
    let nativo = norte_vfs::native::vpath_to_native(desde).unwrap_or_else(|_| {
        std::env::var_os("HOME")
            .map_or_else(|| std::path::PathBuf::from("/"), std::path::PathBuf::from)
    });
    for argv in norte_frontend::shell::directory_picker_candidates(&nativo) {
        let (programa, args) = argv.split_first()?;
        let salida = std::process::Command::new(programa)
            .args(args)
            // Sin stdin: un selector no lee nada, y dejárselo abierto es una
            // puerta que no hace falta (misma regla 9 que el resto).
            .stdin(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .output();
        let Ok(salida) = salida else {
            // No está en el PATH: al siguiente.
            continue;
        };
        if !salida.status.success() {
            // Existe y se cerró sin elegir. NO se prueba el siguiente: el
            // lector ya contestó, y abrirle otro selector sería no aceptar un
            // «no».
            return None;
        }
        let ruta = String::from_utf8_lossy(&salida.stdout)
            .trim_end()
            .to_owned();
        return (!ruta.is_empty()).then_some(ruta);
    }
    None
}

/// Escribe `bytes` en el portapapeles con el primer helper que exista.
///
/// El cuerpo vive en `norte_frontend::shell` desde #286: el terminal necesita
/// exactamente lo mismo, y tener dos copias de «qué helper y en qué orden»
/// es tener dos respuestas a la misma pregunta. Lo que esta ventana NO tiene
/// es la salida de OSC 52, que necesita un emulador de terminal delante.
fn copiar(bytes: &[u8]) -> Resultado {
    match norte_frontend::shell::copy_to_clipboard(bytes) {
        norte_frontend::shell::ClipboardOutcome::Done(_) => Resultado::Hecho,
        norte_frontend::shell::ClipboardOutcome::NoHelper => Resultado::SinPrograma,
        norte_frontend::shell::ClipboardOutcome::Failed => Resultado::Fallo,
    }
}

/// Abre `path` con la aplicación que el escritorio elija.
fn abrir(path: &norte_proto::VPath) -> Resultado {
    let Ok(nativa) = norte_vfs::native::vpath_to_native(path) else {
        // El host ya lo comprueba; aquí es el cinturón: a `xdg-open` no se le
        // da algo que no está en este disco.
        return Resultado::SinPrograma;
    };
    let (programa, argv) = norte_frontend::openers::system_opener(&nativa);
    let Some(ruta) = norte_frontend::openers::resolve_program(std::ffi::OsStr::new(&programa))
    else {
        return Resultado::SinPrograma;
    };
    lanzar(&ruta, &argv[1..], None)
}

/// Abre un terminal sentado en `dir`.
fn terminal(dir: &norte_proto::VPath) -> Resultado {
    let Ok(nativa) = norte_vfs::native::vpath_to_native(dir) else {
        return Resultado::SinPrograma;
    };
    for argv in norte_frontend::shell::terminal_candidates(&nativa) {
        let Some(programa) = argv.first() else {
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
