//! La capa WASM: los bindings del world `norte-panel` y nada más.
//!
//! Todo lo que decide algo vive en [`crate`] y se prueba en el host. Aquí solo
//! se traduce: el token del host a lecturas relativas, y el estado leído a
//! líneas con estilo y zonas pulsables.

extern crate alloc;

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

// El world va CUALIFICADO con su paquete: el `wit` de aquí es un symlink al
// del host, cuyo paquete raíz es `norte:plugin`, y `norte:panel` es una
// dependencia suya. Sin el prefijo, wit-bindgen busca el world en la raíz y no
// lo encuentra. Mismo camino que `norte:renamer/norte-renamer` en date-prefix.
wit_bindgen::generate!({
    world: "norte:panel/norte-panel",
    path: "wit",
    generate_all,
});

use exports::norte::panel::panel::{
    Frame, Guest as PanelGuest, Hit, LocationRef, PanelContext, PanelEvent, Span,
};
use norte::host::{host_config, host_log};
use norte::location::location;

/// Un tramo con ROL del tema: el color lo elige el tema del lector, no este
/// plugin. Es lo que hace que el panel se vea como el resto de norte en
/// cualquier tema, claro u oscuro.
fn rol(text: &str, role: &str) -> Span {
    Span {
        text: text.to_string(),
        role: Some(role.to_string()),
        fg: None,
        bg: None,
    }
}

/// Un tramo sin estilo: el color por defecto del panel.
fn llano(text: &str) -> Span {
    Span {
        text: text.to_string(),
        role: None,
        fg: None,
        bg: None,
    }
}

/// Cuántos movimientos pide la configuración. Un valor que no es un número no
/// es un error: se usa el de fábrica, que es lo que hace el resto del host con
/// una clave mal escrita.
fn tope_de_movimientos() -> usize {
    host_config::get("moves")
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(crate::MOVES_DEFAULT)
}

/// Una línea suelta, para los casos en los que no hay repositorio que contar.
fn solo_una_linea(texto: &str, state: Vec<u8>) -> Frame {
    Frame {
        lines: vec![vec![rol(texto, "muted")]],
        hits: Vec::new(),
        state,
    }
}

struct GitPanel;

impl PanelGuest for GitPanel {
    fn render(
        kind: String,
        context: PanelContext,
        location: Option<LocationRef>,
        state: Vec<u8>,
        event: PanelEvent,
    ) -> Result<Frame, String> {
        if kind != crate::PANEL_KIND {
            return Err(format!("este plugin no pinta «{kind}»"));
        }
        // El evento no cambia lo que se pinta: este panel describe el
        // repositorio, y el repositorio no depende de dónde se pulsó. Se
        // registra el comando porque es lo único que un panel puede recibir
        // hoy sin que el lector lo vea, y un plugin que reacciona en silencio
        // es un plugin que nadie puede depurar.
        if let PanelEvent::Command(cmd) = &event {
            host_log::log(&format!("git-panel: comando {cmd}"));
        }

        // Sin ubicación aprobada no hay nada que leer, y decirlo es la
        // respuesta correcta: el hueco sigue pintándose, con una línea que
        // explica por qué está vacío en vez de quedarse en blanco.
        let Some(loc) = location else {
            return Ok(solo_una_linea("sin permiso de lectura", state));
        };
        let Ok(head) = location::read(&loc.token, b".git/HEAD") else {
            // La raíz que el host abrió no tiene `.git`: no es un repositorio,
            // o la ubicación no es local (sftp, s3, dentro de un archivo).
            return Ok(solo_una_linea("aquí no hay un repositorio", state));
        };

        let rama = crate::rama_de_head(&head);
        let reflog = location::read(&loc.token, b".git/logs/HEAD").unwrap_or_default();
        let (commit, movimientos) = crate::del_reflog(&reflog, tope_de_movimientos());

        let mut lines: Vec<Vec<Span>> = Vec::new();
        let mut hits: Vec<Hit> = Vec::new();

        // La rama, en la primera línea: es el dato que se mira de un vistazo.
        // `HEAD` desprendido se dice tal cual, porque es un estado en el que
        // se hacen cosas que luego se pierden.
        let etiqueta = "rama ";
        match &rama {
            Some(nombre) => lines.push(vec![rol(etiqueta, "muted"), rol(nombre, "title")]),
            None => lines.push(vec![
                rol(etiqueta, "muted"),
                rol("(HEAD desprendido)", "warning"),
            ]),
        }

        if let Some(sha) = &commit {
            lines.push(vec![rol("commit ", "muted"), llano(sha)]);
        } else {
            lines.push(vec![rol("sin commits todavía", "muted")]);
        }

        if !movimientos.is_empty() {
            lines.push(Vec::new());
            lines.push(vec![rol("últimos movimientos", "muted")]);
            for m in &movimientos {
                // El motivo se recorta al ancho del panel: el guest sabe
                // cuántas columnas tiene (`context.cols`), y una línea que el
                // host tuviera que cortar diría menos que una que ya cabe.
                let ancho = (context.cols as usize).saturating_sub(15).max(8);
                let motivo: String = m.reason.chars().take(ancho).collect();
                lines.push(vec![llano(&m.to), llano(" "), rol(&motivo, "muted")]);
            }
        }

        // Una zona pulsable, y una sola: abrir el panel de registro, que es
        // donde se ve lo que norte hizo con el repositorio. El comando es del
        // catálogo y está dentro de lo que una zona puede nombrar; cualquier
        // otro lo rehusaría el host, y con razón.
        let pie = "[registro]";
        let fila = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        lines.push(vec![rol(pie, "muted")]);
        hits.push(Hit {
            row: fila,
            col: 0,
            width: u16::try_from(pie.chars().count()).unwrap_or(u16::MAX),
            command: "layout.log".to_string(),
            arg: None,
        });

        // Nada que recordar entre repintados: lo que se enseña se lee entero
        // cada vez, y son dos ficheros pequeños. Devolver el estado que llegó
        // sería fingir una continuidad que no existe.
        Ok(Frame {
            lines,
            hits,
            state: Vec::new(),
        })
    }
}

export!(GitPanel);
