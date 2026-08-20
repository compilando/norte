//! Qué comandos sabe ejecutar este host, y qué hace con los que no.
//!
//! La lista importa por dos motivos. Uno: es lo que el keymap efectivo
//! necesita para decidir si una tecla ligada puede ejecutarse AQUÍ
//! (`Availability::NotHere` es «este frontend no lo implementa», y sin la
//! lista no se puede distinguir de «norte no lo ha construido»). Y dos: es
//! la única declaración honesta de hasta dónde llega el host, en vez de un
//! `match` que se traga en silencio lo que no reconoce.

/// Hasta dónde llega un frontend: si puede MUTAR o solo mirar.
///
/// No es una amputación del host —el host sabe borrar y crear, y sus tests lo
/// prueban— sino una decisión de ARRANQUE de quien lo monta. La ventana
/// gráfica arranca en solo lectura hasta que la fase 5 le dé el camino seguro
/// (el gate de salida de la fase 4 lo exige), y hasta entonces una tecla
/// atada a `pane.delete` en el preset se responde en vez de ejecutarse: que
/// la tecla exista no es permiso.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Efectos {
    /// Solo mirar: navegar, marcar, ordenar, ver. Nada que escriba, y
    /// tampoco aprobar que escriba un agente.
    SoloLectura,
    /// Todo lo que el host implementa.
    Completo,
}

/// Los comandos que el host ejecuta en cada modo.
///
/// La lista de solo lectura es la de siempre MENOS lo que muta; se deriva de
/// una sola fuente para que añadir un comando destructivo no se olvide de
/// quitarlo aquí.
#[must_use]
pub fn implementados(efectos: Efectos) -> Vec<&'static str> {
    match efectos {
        Efectos::Completo => IMPLEMENTADOS.to_vec(),
        Efectos::SoloLectura => IMPLEMENTADOS
            .iter()
            .copied()
            .filter(|c| !MUTAN.contains(c))
            .collect(),
    }
}

/// Los comandos de [`IMPLEMENTADOS`] que ESCRIBEN.
pub const MUTAN: &[&str] = &["pane.mkdir", "pane.delete", "pane.delete-permanent"];

/// Los comandos que el host ejecuta HOY.
///
/// Crece con cada tarea de la fase 2. Todo lo demás del catálogo resuelve a
/// [`norte_frontend::keymap::Availability::NotHere`] y se DICE en la barra,
/// que es exactamente lo que hace el TUI con los suyos.
pub const IMPLEMENTADOS: &[&str] = &[
    "cursor.up",
    "cursor.down",
    "cursor.page-up",
    "cursor.page-down",
    "cursor.top",
    "cursor.bottom",
    "nav.enter",
    "nav.parent",
    "nav.back",
    "nav.forward",
    "mark.toggle",
    "mark.clear",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.set-target",
    "pane.view",
    "pane.quick-search",
    "pane.mkdir",
    "pane.delete",
    "pane.delete-permanent",
];

/// Los comandos de la pantalla del VISOR que el host ejecuta.
///
/// Lista aparte porque es otra pantalla, y su keymap efectivo se construye
/// con `Screen::Viewer`: un comando que no esté aquí resuelve a
/// [`norte_frontend::keymap::Availability::NotHere`] y se DICE, igual que en
/// el listado.
pub const IMPLEMENTADOS_VISOR: &[&str] = &[
    "viewer.close",
    "viewer.up",
    "viewer.down",
    "viewer.page-up",
    "viewer.page-down",
    "viewer.top",
    "viewer.bottom",
    "viewer.hex",
    "viewer.encoding",
    "viewer.encoding-auto",
];

/// Todo lo que el host implementa, en las dos pantallas.
///
/// Es lo que se le pasa a `Effective::build_for` en AMBAS: el keymap efectivo
/// necesita saber qué existe para poder distinguir «este frontend no lo hace»
/// de «norte no lo tiene», y esa pregunta no es por pantalla.
#[must_use]
pub fn todos() -> Vec<&'static str> {
    todos_con(Efectos::Completo)
}

/// Igual, con el modo de efectos dicho.
#[must_use]
pub fn todos_con(efectos: Efectos) -> Vec<&'static str> {
    let mut v = implementados(efectos);
    v.extend_from_slice(IMPLEMENTADOS_VISOR);
    v
}

/// Lo que un comando del VISOR le pide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EfectoVisor {
    /// Cierra el visor.
    Cerrar,
    /// Desplaza tantas líneas (negativo hacia arriba).
    Linea(i64),
    /// Desplaza tantas PÁGINAS (negativo hacia arriba).
    Pagina(i64),
    /// Al principio o al final.
    Extremo {
        /// `true` = al final.
        al_final: bool,
    },
    /// Alterna el hexadecimal.
    Hex,
    /// Recarga con el siguiente encoding del ciclo.
    Encoding,
    /// Vuelve a la detección automática.
    EncodingAuto,
}

/// Traduce un comando de la pantalla del visor a su efecto.
///
/// `None` = el host no lo implementa; quien llama lo convierte en un
/// `Unavailable` que el usuario ve.
#[must_use]
pub fn efecto_visor_de(command: &str, veces: u32) -> Option<EfectoVisor> {
    let n = i64::from(veces.max(1).min(u32::from(u16::MAX)));
    Some(match command {
        "viewer.close" => EfectoVisor::Cerrar,
        "viewer.up" => EfectoVisor::Linea(-n),
        "viewer.down" => EfectoVisor::Linea(n),
        "viewer.page-up" => EfectoVisor::Pagina(-n),
        "viewer.page-down" => EfectoVisor::Pagina(n),
        "viewer.top" => EfectoVisor::Extremo { al_final: false },
        "viewer.bottom" => EfectoVisor::Extremo { al_final: true },
        "viewer.hex" => EfectoVisor::Hex,
        "viewer.encoding" => EfectoVisor::Encoding,
        "viewer.encoding-auto" => EfectoVisor::EncodingAuto,
        _ => return None,
    })
}

/// Lo que un comando le pide al hueco con el foco.
///
/// Es el vocabulario INTERNO del host: el renderer nunca lo ve. Existe para
/// que el resolver y el ratón acaben en el mismo sitio — un gesto y una
/// tecla que significan lo mismo tienen que hacer lo mismo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Efecto {
    /// Mueve el cursor tantas filas (negativo hacia arriba).
    Cursor(i64),
    /// Mueve el cursor tantas PÁGINAS (negativo hacia arriba). Cuántas filas
    /// son lo decide el hueco con la ventana que el renderer le dijo.
    Pagina(i64),
    /// Cursor al principio o al final del listado.
    Extremo {
        /// `true` = al final.
        al_final: bool,
    },
    /// Entra en lo que haya bajo el cursor.
    Entrar,
    /// Sube al directorio padre.
    Subir,
    /// Rastro de navegación.
    Rastro {
        /// `true` = atrás.
        atras: bool,
    },
    /// Marca o desmarca la fila del cursor.
    Marcar,
    /// Quita todas las marcas.
    DesmarcarTodo,
    /// Mueve el foco al siguiente hueco enfocable (o al anterior).
    ///
    /// Con dos paneles es el cambio de siempre; con más, sigue el ORDEN de
    /// tabulación que resuelve la capa compartida, que ya se salta lo que no
    /// se ve y lo que no se enfoca.
    Foco {
        /// `true` = hacia atrás.
        atras: bool,
    },
    /// Designa OTRO hueco como destino de la siguiente operación.
    Destino,
    /// Abre el visor sobre la entrada bajo el cursor.
    Ver,
    /// Abre el buscador incremental del listado.
    BuscarRapido,
    /// Abre el prompt de crear directorio.
    CrearDirectorio,
    /// Pide borrar lo marcado (o lo que haya bajo el cursor). NO borra: abre
    /// la confirmación, que es por donde pasan TODAS las vías —tecla, menú,
    /// gesto—, porque una operación destructiva con dos puertas acaba
    /// teniendo una sin cerrojo.
    Borrar {
        /// Permanente, sin papelera.
        permanente: bool,
    },
}

/// Traduce un comando del catálogo al efecto que el host aplica.
///
/// `None` = el host no lo implementa. No es un descarte silencioso: quien
/// llama lo convierte en un `Unavailable` que el usuario ve.
#[must_use]
pub fn efecto_de(command: &str, veces: u32) -> Option<Efecto> {
    let n = i64::from(veces.max(1).min(u32::from(u16::MAX)));
    Some(match command {
        "cursor.up" => Efecto::Cursor(-n),
        "cursor.down" => Efecto::Cursor(n),
        // Una página son las filas VISIBLES, y cuántas son lo sabe el hueco
        // (el renderer se lo dijo con `SetVisibleRange`): por eso viaja como
        // páginas y no como filas.
        "cursor.page-up" => Efecto::Pagina(-n),
        "cursor.page-down" => Efecto::Pagina(n),
        "cursor.top" => Efecto::Extremo { al_final: false },
        "cursor.bottom" => Efecto::Extremo { al_final: true },
        "nav.enter" => Efecto::Entrar,
        "nav.parent" => Efecto::Subir,
        "nav.back" => Efecto::Rastro { atras: true },
        "nav.forward" => Efecto::Rastro { atras: false },
        "mark.toggle" => Efecto::Marcar,
        "mark.clear" => Efecto::DesmarcarTodo,
        // `pane.switch` es el cambio clásico entre dos paneles; con más de
        // dos, lo honesto es seguir el mismo recorrido que el tabulador en
        // vez de inventar un segundo orden.
        "pane.switch" | "layout.focus-next" => Efecto::Foco { atras: false },
        "layout.focus-prev" => Efecto::Foco { atras: true },
        "layout.set-target" => Efecto::Destino,
        "pane.view" => Efecto::Ver,
        "pane.quick-search" => Efecto::BuscarRapido,
        "pane.mkdir" => Efecto::CrearDirectorio,
        "pane.delete" => Efecto::Borrar { permanente: false },
        "pane.delete-permanent" => Efecto::Borrar { permanente: true },
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Todo lo que se declara implementado tiene efecto, y al revés. Sin
    /// esto, la lista y el `match` se separan y `Availability` empieza a
    /// mentir.
    #[test]
    fn la_lista_y_los_efectos_no_pueden_separarse() {
        for c in IMPLEMENTADOS {
            assert!(
                efecto_de(c, 1).is_some(),
                "{c} está en la lista y no tiene efecto"
            );
        }
    }

    /// Lo mismo para la pantalla del visor.
    #[test]
    fn la_lista_del_visor_y_sus_efectos_no_pueden_separarse() {
        for c in IMPLEMENTADOS_VISOR {
            assert!(
                efecto_visor_de(c, 1).is_some(),
                "{c} está en la lista del visor y no tiene efecto"
            );
        }
    }

    /// Y las dos listas son disjuntas: un comando en las dos significaría que
    /// una tecla hace dos cosas distintas según la pantalla sin que nadie lo
    /// declare.
    #[test]
    fn las_dos_pantallas_no_comparten_comandos() {
        for c in IMPLEMENTADOS_VISOR {
            assert!(
                !IMPLEMENTADOS.contains(c),
                "{c} está declarado en las dos pantallas"
            );
        }
    }

    /// Y todo lo declarado existe en el catálogo compartido: un comando
    /// inventado aquí no lo ligaría ningún preset.
    #[test]
    fn todo_lo_declarado_esta_en_el_catalogo() {
        for c in todos() {
            assert!(
                norte_frontend::keymap::CATALOGUE
                    .iter()
                    .any(|d| d.name == c),
                "{c} no está en el catálogo compartido"
            );
        }
    }

    /// Lo que muta está DENTRO de lo implementado: una lista de mutaciones
    /// con un comando que el host no ejecuta sería un filtro que no filtra.
    #[test]
    fn lo_que_muta_es_un_subconjunto_de_lo_implementado() {
        for c in MUTAN {
            assert!(IMPLEMENTADOS.contains(c), "{c} no está implementado");
        }
        let solo_lectura = implementados(Efectos::SoloLectura);
        for c in MUTAN {
            assert!(!solo_lectura.contains(c), "{c} sobrevive a solo lectura");
        }
        assert_eq!(
            solo_lectura.len() + MUTAN.len(),
            IMPLEMENTADOS.len(),
            "solo lectura quita EXACTAMENTE lo que muta"
        );
    }

    /// El contador multiplica lo que se puede repetir.
    #[test]
    fn el_contador_multiplica() {
        assert_eq!(efecto_de("cursor.down", 3), Some(Efecto::Cursor(3)));
        assert_eq!(efecto_de("cursor.up", 3), Some(Efecto::Cursor(-3)));
        assert_eq!(efecto_de("cursor.page-down", 2), Some(Efecto::Pagina(2)));
    }
}
