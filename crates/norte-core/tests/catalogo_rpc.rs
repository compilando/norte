//! Cada método del catálogo LLEGA a las superficies que le tocan (ADR 0089).
//!
//! El catálogo (`norte_proto::catalog`) dice qué métodos existen. Aquí se le
//! pregunta a cada superficie si está: el reparto del daemon, el cliente
//! remoto, y el agregado del schema.
//!
//! Vive en `norte-core` y no en `norte-proto` porque es aquí donde se pueden
//! leer los tres ficheros. Se leen como TEXTO a propósito: comprobarlo con
//! tipos exigiría que el reparto plano del daemon dejara de ser plano, y ese
//! reparto es deliberado — un `match` de cien brazos donde cada brazo se lee
//! entero es mejor que diez capas que hay que recorrer para saber qué hace
//! `fs.stat`. Lo que faltaba no era estructura, era que olvidar uno se notara.
//!
//! Lo que este test NO puede decir es si el brazo hace lo correcto. Dice que
//! existe. Es exactamente la clase de olvido que se colaba.

use norte_proto::catalog::{CATALOGO, Kind, MethodInfo, Shape};

/// Lee un fichero del workspace desde la raíz del crate.
fn fuente(rel: &str) -> String {
    let ruta = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&ruta).unwrap_or_else(|e| panic!("se lee {}: {e}", ruta.display()))
}

/// El nombre de la constante a partir del de wire (`fs.stat` → `FS_STAT`).
///
/// Se busca la CONSTANTE y no la cadena: el daemon y el cliente nombran
/// `methods::FS_STAT`, nunca `"fs.stat"` a pelo, y buscar la cadena daría
/// falsos negativos en todos ellos.
fn constante_de(m: &MethodInfo) -> String {
    m.name.replace('.', "_").to_uppercase()
}

/// ¿Aparece `methods::K` como identificador COMPLETO?
///
/// No con `contains`, que era el fallo: `methods::SYNC_PLAN` es subcadena de
/// `methods::SYNC_PLAN_DONE`, así que borrar el brazo de `sync.plan` dejaba el
/// test verde. Le pasaba a unos quince métodos —todos los que tienen una
/// constante hermana más larga: `FS_CHECKSUM`/`_REPORT`,
/// `PLUGIN_PREVIEW`/`_STYLED`, `FS_READ`/`FS_READ_MAX_CHUNK`…— o sea que la
/// comprobación era indicativa y no falsable justo donde más falta hacía.
fn nombra(src: &str, k: &str) -> bool {
    let aguja = format!("methods::{k}");
    src.match_indices(&aguja).any(|(i, _)| {
        let siguiente = src[i + aguja.len()..].chars().next();
        !siguiente.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// ¿Tiene BRAZO de reparto (`methods::K =>`)?
///
/// Es lo que distingue una petición de una notificación en el daemon, y lo
/// que impide que el nombre cuente por aparecer en un doc-comment o en la
/// lista de cancelables. `RPC_CANCEL` estaba catalogado como petición y no
/// tiene brazo: con esta comprobación habría salido rojo el primer día.
fn tiene_brazo(src: &str, k: &str) -> bool {
    let aguja = format!("methods::{k}");
    src.match_indices(&aguja).any(|(i, _)| {
        let resto = src[i + aguja.len()..].trim_start();
        resto.starts_with("=>")
    })
}

/// El texto del BRAZO de reparto de `k`: desde `methods::K =>` hasta el
/// principio del brazo siguiente.
///
/// Trocear por brazos es lo que hace `shape` falsable. Sin esto era el único
/// campo del catálogo que nadie comprobaba — y mintió el primer día:
/// `index.build` estaba declarado `Direct` con `IndexBuildResult` cuando el
/// daemon registra una Task y contesta `FsTaskResult`.
fn brazo_de<'a>(src: &'a str, k: &str) -> Option<&'a str> {
    let aguja = format!("methods::{k}");
    let inicio = src
        .match_indices(&aguja)
        .find(|(i, _)| src[i + aguja.len()..].trim_start().starts_with("=>"))?
        .0;
    let resto = &src[inicio + aguja.len()..];
    // El brazo acaba en lo PRIMERO de estas tres: el siguiente `methods::… =>`,
    // el brazo comodín, o el final de la función. Sin las dos últimas, el
    // último brazo de cada `match` se tragaba el resto del fichero y arrastraba
    // el `FsTaskResult` de cualquier función de más abajo — que es como
    // `connection.provide_secret` y `plugin.set_config` salieron marcados sin
    // registrar nada.
    let siguiente_brazo = resto.match_indices("methods::").find(|(i, _)| {
        let tras = &resto[*i + "methods::".len()..];
        let ident: String = tras
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        !ident.is_empty()
            && resto[*i + "methods::".len() + ident.len()..]
                .trim_start()
                .starts_with("=>")
    });
    let fin = [
        siguiente_brazo.map(|(i, _)| i),
        resto.find("other =>"),
        resto.find("\n}"),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(resto.len());
    Some(&resto[..fin])
}

/// **Un método `Task` o `Stream` registra una Task; uno `Direct` no.**
///
/// Es la comprobación que hace de `shape` algo que se puede desmentir. Los
/// brazos de reparto que devuelven `task_id` lo hacen por `register_task` o
/// construyendo un `FsTaskResult`; los directos no hacen ninguna de las dos.
#[test]
fn el_shape_del_catalogo_casa_con_lo_que_hace_el_daemon() {
    let src = fuente("src/daemon/server.rs");
    let mut mal = Vec::new();
    for m in CATALOGO.iter().filter(|m| m.kind == Kind::Request) {
        let Some(brazo) = brazo_de(&src, &constante_de(m)) else {
            continue; // Lo cubre `el_daemon_reparte_todas_las_peticiones`.
        };
        // Se mira el brazo Y la función que llama: el brazo suele ser una
        // línea que delega, así que un `Task` se reconoce por devolver
        // `FsTaskResult` en cualquiera de los dos sitios.
        // `FsTaskResult` o `register_task`, y NO un `task_id` suelto: hay
        // métodos directos que RECIBEN un `task_id` como parámetro
        // (`task.cancel`, los informes) y no registran nada.
        let es_task = brazo.contains("FsTaskResult") || brazo.contains("register_task");
        let declarado_task = matches!(m.shape, Shape::Task | Shape::Stream);
        // Solo se afirma en la dirección segura: un brazo que delega a una
        // función puede no decir nada aquí, y eso no es una mentira. Lo que
        // SÍ lo es: declararse `Direct` y registrar una Task a la vista.
        if es_task && !declarado_task {
            mal.push(format!("{} dice Direct y registra una Task", m.name));
        }
    }
    assert!(
        mal.is_empty(),
        "el catálogo no dice lo que hace el daemon: {mal:?}"
    );
}

/// Los métodos que una superficie no tiene por qué nombrar, con su motivo.
///
/// Una lista de excepciones es una deuda: cada entrada dice por qué NO se
/// comprueba algo, y sin motivo escrito no entra.
struct Excepcion {
    metodo: &'static str,
    motivo: &'static str,
}

/// **El reparto del daemon nombra todas las peticiones.**
#[test]
fn el_daemon_reparte_todas_las_peticiones() {
    let src = fuente("src/daemon/server.rs");
    // Las notificaciones no se reparten: las EMITE el daemon, y también
    // aparecen en este fichero, así que se comprueban igual más abajo.
    let mut faltan = Vec::new();
    for m in CATALOGO.iter().filter(|m| m.kind == Kind::Request) {
        if !tiene_brazo(&src, &constante_de(m)) {
            faltan.push(m.name);
        }
    }
    assert!(
        faltan.is_empty(),
        "peticiones que el daemon no reparte: {faltan:?}.\n\
         Un método declarado que el daemon no atiende contesta \
         METHOD_NOT_FOUND a un cliente que lo cree soportado."
    );
}

/// **Y una notificación NO tiene brazo de reparto.**
///
/// La otra mitad, que es la que ata `kind` en vez de dejarlo a mi palabra: si
/// algo catalogado como notificación se repartiera como petición, o al revés,
/// una de las dos comprobaciones se cae. Es lo que cazó que `rpc.cancel`
/// estuviera catalogado como petición.
#[test]
fn una_notificacion_no_se_reparte_como_peticion() {
    let src = fuente("src/daemon/server.rs");
    let mut sobran = Vec::new();
    for m in CATALOGO.iter().filter(|m| m.kind == Kind::Notification) {
        if tiene_brazo(&src, &constante_de(m)) {
            sobran.push(m.name);
        }
    }
    assert!(
        sobran.is_empty(),
        "catalogadas como notificación pero el daemon las reparte como \
         petición: {sobran:?}. Una de las dos cosas es mentira."
    );
}

/// **El daemon emite todas las notificaciones que el catálogo declara.**
#[test]
fn el_daemon_emite_todas_las_notificaciones() {
    let src = [
        fuente("src/daemon/server.rs"),
        fuente("src/daemon/mod.rs"),
        fuente("src/engine.rs"),
        // `rpc.cancel` la manda el CLIENTE al soltar una petición, no el
        // daemon: es la única notificación que va en esa dirección.
        fuente("../norte-client/src/remote/calls.rs"),
    ]
    .join("\n");
    let mut faltan = Vec::new();
    for m in CATALOGO.iter().filter(|m| m.kind == Kind::Notification) {
        if !nombra(&src, &constante_de(m)) {
            faltan.push(m.name);
        }
    }
    assert!(
        faltan.is_empty(),
        "notificaciones que nadie emite: {faltan:?}.\n\
         Una notificación declarada que no se manda es una pantalla que espera \
         algo que no va a llegar."
    );
}

/// **El cliente remoto sabe pedir todo lo que el catálogo declara.**
///
/// Es la superficie que más silenciosamente se olvida: el daemon atiende el
/// método, el schema lo publica, y la ventana no tiene por dónde llamarlo.
#[test]
fn el_cliente_remoto_sabe_pedirlo_todo() {
    // Los CUATRO ficheros del cliente remoto. Leer solo dos hacía que la
    // comprobación de excepciones caducas mintiera: afirmaba que el cliente
    // no pedía `rpc.cancel` cuando lo manda en `calls.rs`.
    let src = [
        fuente("../norte-client/src/remote/mod.rs"),
        fuente("../norte-client/src/remote/paging.rs"),
        fuente("../norte-client/src/remote/calls.rs"),
        fuente("../norte-client/src/remote/routes.rs"),
    ]
    .join("\n");

    // El cliente NO es el daemon: hay métodos que por diseño no le tocan.
    let excepciones = [
        Excepcion {
            metodo: "daemon.shutdown",
            motivo: "apagar el daemon es un acto del CLI, no del SDK que lo usa",
        },
        Excepcion {
            metodo: "policy.request_scope",
            motivo: "lo pide un AGENTE por MCP, no una ventana",
        },
        Excepcion {
            // Lo destapó este test en su primera pasada, y resultó no ser un
            // olvido: se concede desde el TERMINAL (`norte policy grant`,
            // `norte-cli/src/main.rs`), con el cliente de bajo nivel del
            // daemon y no con el SDK. Que el hueco existiera a propósito no
            // estaba escrito en ninguna parte; ahora sí.
            metodo: "policy.grant_scope",
            motivo: "conceder un scope a un agente es un acto deliberado del \
                     CLI; ninguna ventana lo ofrece",
        },
    ];

    let mut faltan = Vec::new();
    for m in CATALOGO.iter().filter(|m| m.kind == Kind::Request) {
        if excepciones.iter().any(|e| e.metodo == m.name) {
            continue;
        }
        if !nombra(&src, &constante_de(m)) {
            faltan.push(m.name);
        }
    }
    assert!(
        faltan.is_empty(),
        "peticiones que el cliente remoto no sabe hacer: {faltan:?}.\n\
         Si es a propósito, entra en `excepciones` CON su motivo; si no, es un \
         método que el daemon atiende y por el que ningún frontend puede \
         preguntar."
    );

    // Una excepción que ya no hace falta es deuda que se queda: si el cliente
    // aprendió a pedirlo, se quita de la lista.
    for e in &excepciones {
        let Some(m) = norte_proto::catalog::buscar(e.metodo) else {
            panic!("la excepción `{}` nombra un método que no existe", e.metodo);
        };
        assert!(
            !nombra(&src, &constante_de(m)),
            "`{}` está exceptuado ({}) pero el cliente SÍ lo pide: quita la excepción",
            e.metodo,
            e.motivo
        );
    }
}

/// **Los tipos del catálogo están en el agregado del schema.**
///
/// El schema publicado se genera de un `struct` con un campo por tipo de
/// wire, escrito a mano. Un método nuevo cuyo `Params` no entre ahí queda
/// fuera del schema publicado sin que nada se ponga rojo.
#[test]
fn los_tipos_del_catalogo_estan_en_el_schema() {
    let src = fuente("../norte-proto/tests/schema.rs");
    let mut faltan = Vec::new();
    for m in CATALOGO {
        for ty in [m.params(), m.result()].into_iter().flatten() {
            // `methods::FsStatParams` → `FsStatParams`, que es como el
            // agregado lo nombra (con o sin prefijo de módulo).
            let corto = ty.rsplit("::").next().unwrap_or(ty).trim();
            if !src.contains(corto) {
                faltan.push(format!("{} → {corto}", m.name));
            }
        }
    }
    faltan.sort();
    faltan.dedup();
    assert!(
        faltan.is_empty(),
        "tipos del catálogo que no entran en el agregado del schema: {faltan:?}.\n\
         Lo que no está en `ProtocolSchema` no sale publicado, y quien \
         implemente el protocolo desde el schema no sabrá que existe."
    );
}
