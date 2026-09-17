//! El backend de tabla que comparten los tests: un árbol de directorios
//! determinista, sin daemon y sin red.
//!
//! Cada test usa la parte que necesita —el de paridad no borra, el del
//! controlador no compara árboles— así que aquí sobra código para cualquiera
//! de ellos por separado. Es el precio de tener UN falso y no tres.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures::future::BoxFuture;
use norte_proto::{DeleteMode, Entry, EntryKind, Error, VPath};
use norte_ui_host::backend::{HostBackend, HostTask};

/// Un pestillo de un solo sentido: se abre una vez y se queda abierto.
///
/// `notify_waiters` solo despierta a quien YA espera, así que la bandera es
/// la que manda y el aviso solo evita el sondeo. Una vez abierta, cualquier
/// petición posterior pasa de largo — que es lo que hace falta cuando el
/// host repide el listado y el stream nuevo vuelve a llegar aquí.
#[derive(Default)]
pub struct Puerta {
    abierta: std::sync::atomic::AtomicBool,
    aviso: tokio::sync::Notify,
}

impl Puerta {
    /// Deja pasar el drenaje, ahora y para siempre.
    pub fn abrir(&self) {
        self.abierta.store(true, Ordering::SeqCst);
        self.aviso.notify_waiters();
    }

    async fn esperar(&self) {
        loop {
            if self.abierta.load(Ordering::SeqCst) {
                return;
            }
            // El futuro se arma ANTES de la segunda comprobación: armarlo
            // después perdería un `abrir` que cayera justo en medio.
            let esperando = self.aviso.notified();
            if self.abierta.load(Ordering::SeqCst) {
                return;
            }
            esperando.await;
        }
    }
}

/// Un backend de tabla: para cada directorio, los nombres que contiene y de
/// qué clase son.
//
// `clippy::struct_excessive_bools`: permitido a propósito. Son MANDOS
// independientes de un doble de test —el listado viene perezoso, el borrado
// quita de verdad, el provider escribe el padre distinto— y cualquier
// combinación de ellos es un escenario real. Plegarlos en una máquina de
// estados sería inventar estados que no existen; envolver cada uno en un enum
// de dos variantes dejaría cada test escribiendo `Lazy::Si, BorrarDeVerdad::No`
// para nada: el nombre del campo ya dice a qué pregunta contesta.
#[derive(Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "hechos independientes: cada campo dice a qué pregunta contesta"
)]
pub struct Falso {
    /// Un aviso por cada cosa que el doble ANOTA.
    ///
    /// Es lo que convierte «duerme 30 ms y mira» en «espera a que pase». El
    /// actor encola cada mutación con `tokio::spawn` y contesta el ack antes
    /// de que la task corra, así que el test que quiera ver lo encolado
    /// tiene que esperar a ALGO; sin esto, ese algo era el reloj.
    ///
    /// `notify_waiters` solo despierta a quien YA espera, así que quien
    /// espera arma el futuro antes de volver a mirar (`Falso::hasta`), igual
    /// que hace `Puerta::esperar`.
    ///
    /// `Arc` porque hay anotaciones que ocurren FUERA de `&self`: el cierre
    /// de cancelación de una task se queda vivo cuando el doble ya no está a
    /// mano, y también tiene que avisar.
    pub pulso: Arc<tokio::sync::Notify>,
    /// `wire del dir` → `(nombre, es_dir)`.
    pub arbol: HashMap<String, Vec<(Vec<u8>, bool)>>,
    /// El kind EXACTO de una entrada, por su ruta de cable. Ver
    /// [`Falso::pon_kind`]: `arbol` solo sabe de directorios y ficheros.
    pub kinds: HashMap<String, EntryKind>,
    pub listados: AtomicUsize,
    /// Lecturas PEDIDAS y ya SERVIDAS: listados, sondeos y contenidos.
    ///
    /// Los dos números solo se separan con `retraso_ms`, y es justo ahí donde
    /// hace falta: un test que quiere ver qué hace el host con una respuesta
    /// TARDÍA tiene que saber cuándo ha llegado. Antes lo adivinaba durmiendo
    /// más que el retraso. `servidos == pedidos` es «ya no vuela ninguna»,
    /// que es la pregunta que esos tests hacen de verdad — y que no exige
    /// contar a mano cuántas respuestas pone en vuelo cada caso.
    pub pedidos: AtomicUsize,
    /// La otra mitad de `pedidos`: `Arc` porque quien la sube es la respuesta,
    /// que corre en su propia task cuando el doble ya no está a mano.
    pub servidos: Arc<AtomicUsize>,
    /// Retraso artificial, para provocar la carrera de una respuesta tardía.
    ///
    /// Este `sleep` se queda: es la latencia que el doble SIMULA, no una
    /// apuesta del test sobre cuánto tarda el actor. Lo que no se adivina es
    /// cuándo acabó — eso lo dice `servidos`.
    pub retraso_ms: u64,
    /// Detiene el stream JUSTO después de la primera página, hasta que el
    /// test la abre.
    ///
    /// Es la única forma de estar DENTRO de la ventana en la que `en_vuelo`
    /// ya se limpió y `drenando` sigue vivo, que es donde vive el bug que
    /// este mando existe para probar. Un `sleep` valdría de casualidad; esto
    /// no depende del reloj.
    pub puerta_drenaje: Option<Arc<Puerta>>,
    /// La sesión que el daemon devuelve, y si esta ventana es su dueña.
    pub sesion: std::sync::Mutex<(norte_proto::methods::Session, bool)>,
    /// Lo ÚLTIMO que se escribió, para comprobar qué guarda el host.
    pub escrito: std::sync::Mutex<Option<serde_json::Value>>,
    /// TODOS los cuerpos que se intentaron poner, en orden — rechazados
    /// incluidos. Es lo que permite ver que un reintento manda algo DISTINTO
    /// (#316), que es la diferencia entre degradar y repetir el mismo error.
    pub puestas: std::sync::Mutex<Vec<serde_json::Value>>,
    /// Cuántos `session_put` seguidos se rechazan por TAMAÑO antes de aceptar
    /// uno. `0` (el defecto) = ninguno.
    pub rechazos_por_tamano: std::sync::Mutex<u32>,
    /// La escritura falla con conflicto: otra ventana escribió en medio.
    pub conflicto: bool,
    /// El listado viene PEREZOSO, como el del provider local: sin tamaño ni
    /// fecha. Quien las quiera, que sondee.
    pub lazy: bool,
    /// El `stat` contesta con el nombre en MAYÚSCULAS: otra ortografía de lo
    /// mismo, como un servidor sin distinción de caja o un HFS+ en NFD.
    pub stat_grita: bool,
    /// Los `attrs` que se pidieron en cada listado, en orden.
    pub attrs_pedidos: std::sync::Mutex<Vec<Vec<String>>>,
    /// Los lotes que pidió `dir_size`, en orden: es lo que permite comprobar
    /// que se cuenta lo MARCADO y en UNA sola Task.
    pub recuentos: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// Lo que se pidió empaquetar, con su formato y su base.
    pub empaquetados: std::sync::Mutex<Vec<norte_proto::methods::ArchivePackParams>>,
    /// Los contenedores que se mandó comprobar.
    pub comprobados: std::sync::Mutex<Vec<norte_proto::methods::ArchiveTestParams>>,
    /// Lo que se mandó partir, con su tamaño de trozo ya en bytes.
    pub partidos: std::sync::Mutex<Vec<norte_proto::methods::FileSplitParams>>,
    /// Los trozos que se mandó juntar.
    pub juntados: std::sync::Mutex<Vec<norte_proto::methods::FileCombineParams>>,
    /// Lo que el daemon contesta a `connection.list` (#264). Por defecto una
    /// lista vacía, que es lo que ve quien no tiene ninguna configurada.
    pub conexiones:
        std::sync::Mutex<Option<Result<Vec<norte_proto::methods::ConnectionEntry>, Error>>>,
    /// Las sesiones que se mandó CERRAR, en orden (#140).
    pub cerradas: std::sync::Mutex<Vec<VPath>>,
    /// Lo que `connection.close` contesta. `None` = «sí, había una».
    pub cierre: std::sync::Mutex<Option<Result<bool, Error>>>,
    /// Contenido por path, para el visor.
    pub contenido: HashMap<String, Vec<u8>>,
    /// Los paths que se sondearon, en orden: es lo que permite comprobar que
    /// un sondeo fallido no se repite en bucle.
    pub sondeos: std::sync::Mutex<Vec<VPath>>,
    /// Lo que se pidió borrar, en orden.
    pub borrados: std::sync::Mutex<Vec<(VPath, DeleteMode)>>,
    /// Un borrado QUITA la entrada del árbol, como en la vida real.
    ///
    /// Apagado por defecto para no mover los tests que solo miran qué se
    /// pidió. Encendido, es lo único que permite comprobar qué hace un
    /// listado que llega con una entrada MENOS — que es donde un cursor por
    /// índice deja de nombrar el mismo fichero.
    pub borrar_de_verdad: bool,
    /// Con qué error se RECHAZA un borrado antes de encolar nada. `None` =
    /// el borrado se encola.
    ///
    /// Tras un `Mutex` para que un test pueda ARREGLARLO a mitad: el caso que
    /// importa es el de un daemon que rehúsa una vez y acepta la siguiente.
    pub error_al_borrar: std::sync::Mutex<Option<Error>>,
    /// Los wire de lo ya borrado, que `list` se salta.
    pub desaparecidos: std::sync::Mutex<std::collections::HashSet<String>>,
    /// El provider escribe el PADRE de sus entradas con otra ortografía que
    /// la que se le pidió (la última componente en mayúsculas).
    ///
    /// Es lo que pasa de verdad en macOS (NFD contra NFC) y contra un
    /// servidor sin distinción de caja, y lo que hace que el padre de una
    /// entrada y el directorio del panel sean dos cadenas para el mismo
    /// sitio.
    pub padre_distinto: bool,
    /// Cuántas veces se pidió cancelar la task que se lanzó.
    pub cancelaciones: Arc<AtomicUsize>,
    /// El emisor del progreso de la última task, para que el test lo mueva.
    pub progreso: std::sync::Mutex<Option<tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// El emisor de la task de `create_file`, guardado solo para que NO se
    /// caiga.
    ///
    /// Es la única del doble que nace corriendo y termina por detrás, así que
    /// es la única cuyo canal tiene que seguir abierto cuando el host va a
    /// leer el desenlace. Aparte de `progreso` porque ese lo MUEVEN los
    /// tests, y esta no la mueve nadie.
    pub progreso_create:
        std::sync::Mutex<Option<tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// Los emisores de TODAS las tasks de transferencia, por id.
    ///
    /// Un solo hueco no vale para un lote: al llegar la segunda se soltaba el
    /// `Sender` de la primera, el bombeo del host veía `changed()` fallar y
    /// esa fila se quedaba `Running` para siempre. O sea que el doble no
    /// podía mover un lote, que es justo el caso caro.
    pub progresos:
        std::sync::Mutex<HashMap<u64, tokio::sync::watch::Sender<norte_proto::TaskProgress>>>,
    /// El catálogo de extensiones que contesta `plugin.list`.
    ///
    /// Tras un `Mutex` porque el gobierno lo CAMBIA: el host repide el
    /// catálogo tras aprobar o encender, y un falso que contestara siempre lo
    /// mismo dejaría pasar una pantalla que dice «aprobada» sin que el
    /// daemon lo hubiera confirmado.
    pub plugins: std::sync::Mutex<Vec<norte_proto::methods::PluginInfo>>,
    /// El `help.md` de cada extensión, por id. Un id ausente contesta como
    /// un daemon que no tiene la página: markdown vacío.
    pub paginas: HashMap<String, String>,
    /// La preview con estilo que contesta un previewer, por wire. Ausente =
    /// ningún previewer aplica, que NO es un error.
    pub previews: HashMap<String, norte_proto::methods::PluginPreviewStyled>,
    /// Las MINIATURAS que un plugin daría por ruta de wire (ADR 0107).
    pub thumbnails: HashMap<String, norte_proto::methods::PluginThumbnail>,
    /// El ancho que cada petición de preview con estilo dijo (0.66.0), en
    /// orden. Es lo que permite comprobar que el viewport CRUZA.
    pub anchos_de_preview: std::sync::Mutex<Vec<Option<u32>>>,
    /// Cuántas entradas dice el provider que se saltó. `None` = no lleva la
    /// cuenta, que NO es lo mismo que cero.
    pub omitidas: Option<u64>,
    /// La insignia que un decorador pone en cada ruta, por wire. Vacío =
    /// NINGÚN decorador consentido, que es lo que contesta el daemon.
    pub decoraciones: HashMap<String, String>,
    /// El ICONO que un segundo decorador, de hueco `icon` (ADR 0105), pone
    /// en cada ruta, por wire. Vacío = ningún decorador de iconos.
    pub iconos: HashMap<String, String>,
    /// Las clases que llegaron con cada lote decorado, en orden: lo que
    /// permite comprobar que la ventana MANDA la clase, sin la que un
    /// decorador de iconos no sabe qué es carpeta.
    pub clases_decoradas: std::sync::Mutex<Vec<Vec<norte_proto::EntryKind>>>,
    /// Los lotes que se pidieron decorar, en orden. Es lo que permite
    /// comprobar que solo se pide la VENTANA.
    pub decorados: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// El valor de una columna de plugin, por `(columna, wire)`.
    pub valores_de_columna: HashMap<(String, String), String>,
    /// Lo que se pidió a `plugin.column_values`, en orden.
    pub columnas_pedidas: std::sync::Mutex<Vec<(String, String, Vec<VPath>)>>,
    /// El marco que contesta `plugin.panel_render` (fase 3). `None` = ningún
    /// plugin consentido pinta ese panel, que es el caso de casi todos los
    /// tests.
    pub marco_de_panel: Option<norte_proto::methods::PanelFrame>,
    /// Lo que se pidió a `plugin.panel_render`, en orden: con esto se
    /// comprueba QUÉ se le cuenta al guest —el directorio, el tamaño sin
    /// marco, la fila bajo el cursor— y que no se le pide dos veces lo mismo.
    pub paneles_pedidos: std::sync::Mutex<Vec<norte_proto::methods::PluginPanelRenderParams>>,
    /// Lo que contesta una búsqueda, por patrón: `(glob, hallazgos)`.
    pub hallazgos: HashMap<String, Vec<VPath>>,
    /// Los patrones que se buscaron, en orden.
    pub busquedas: std::sync::Mutex<Vec<String>>,
    /// Los volúmenes que contesta `host.volumes`.
    pub volumenes: Vec<norte_proto::methods::Volume>,
    /// Cómo PLIEGA nombres cada ubicación (#268/#274). Clave: el wire del
    /// directorio. Ausente = lo que dice `Capabilities::default()`.
    ///
    /// Es el mando que faltaba para poder escribir estos tests: sin él ningún
    /// doble podía fingir un APFS, un NTFS o un exFAT, y las fixtures de
    /// gemelos de caja del corpus no tenían contra qué correr.
    pub capacidades: std::collections::HashMap<String, norte_proto::Capabilities>,
    /// `fs.capabilities` FALLA, así que el hueco no llega a tener ninguna.
    ///
    /// Es el estado que pierde datos si alguien lo confunde con «no hay
    /// papelera», y sin este mando no se podía escribir: el doble siempre
    /// contestaba algo.
    pub error_de_capacidades: bool,
    /// Directorios de plugin que no cargaron: `(dir, motivo)`.
    pub errores_de_carga: Vec<(String, String)>,
    /// Los BYTES del directorio de un error de carga (#265), por su cadena.
    /// Lo que un daemon 0.53 manda; ausente = un peer 0.52.
    pub bytes_de_carga: std::collections::HashMap<String, Vec<u8>>,
    /// El esquema `[config]` de cada extensión, por id.
    pub esquemas: HashMap<String, Vec<norte_proto::methods::PluginConfigKeyWire>>,
    /// Lo que contesta `ai.rename_plan`. `None` = el daemon falla.
    pub plan_ia: Option<Vec<(String, String)>>,
    /// Lo que contesta `plugin.rename_plan` (C3). `None` = el daemon falla.
    pub plan_renamer: Option<Vec<(String, String)>>,
    /// Con qué frase REHÚSA el renamer (#332): gana a `plan_renamer`.
    pub renamer_rehusa: Option<String>,
    /// Qué renamer se pidió, con qué nombres: `(plugin, renamer, nombres)`.
    pub renamers_pedidos: std::sync::Mutex<Vec<(String, String, Vec<String>)>>,
    /// Lo que TARDA el modelo. Es lo que abre la ventana en la que el lector
    /// puede descartar la revisión antes de que llegue el plan.
    pub retraso_ia_ms: u64,
    /// Las instrucciones que se pidieron, en orden.
    pub instrucciones: std::sync::Mutex<Vec<String>>,
    /// Los NOMBRES que viajaron con cada plan (#121): vacío = el directorio
    /// entero. Es lo que permite comprobar que marcar cinco ficheros no manda
    /// los mil del directorio al proveedor.
    pub nombres_ia: std::sync::Mutex<Vec<Vec<String>>>,
    /// El veredicto que contesta `fs.rename_batch_plan`. `None` = falla.
    pub veredicto: Option<norte_proto::methods::FsRenameBatchPlanResult>,
    /// Las parejas con las que se pidió el veredicto, en orden.
    pub veredictos_pedidos: std::sync::Mutex<Vec<Vec<norte_proto::methods::RenamePair>>>,
    /// Los lotes que se mandaron EJECUTAR: `(dir, parejas, hash)`.
    pub lotes: std::sync::Mutex<
        Vec<(
            VPath,
            Vec<norte_proto::methods::RenamePair>,
            norte_proto::methods::PlanHash,
        )>,
    >,
    /// El informe que contesta `fs.rename_batch_report`. `None` = el daemon
    /// no sabe informar (`Unsupported`), que es un caso propio: no se puede
    /// confundir con «el lote fue bien».
    pub informe: std::sync::Mutex<Option<norte_proto::methods::FsRenameBatchReportResult>>,
    /// Los ids de task cuyo informe se pidió, en orden.
    pub informes_pedidos: std::sync::Mutex<Vec<u64>>,
    /// Lo que contesta `sync.plan`: sus pasos y el cierre. `None` = el
    /// método falla con `Unsupported`.
    pub plan_de_sync: std::sync::Mutex<
        Option<(
            Vec<norte_proto::methods::SyncStep>,
            norte_proto::methods::SyncPlanDone,
        )>,
    >,
    /// El informe que contesta `sync.report`. `None` = `Unsupported`.
    pub informe_de_sync: std::sync::Mutex<Option<norte_proto::methods::SyncReportResult>>,
    /// Las sesiones de agente que se pidió deshacer, en orden.
    pub deshechas: std::sync::Mutex<Vec<String>>,
    /// Cuántas veces se ha pedido el catálogo de extensiones.
    pub catalogos_pedidos: std::sync::atomic::AtomicU64,
    /// Con qué DESENLACE termina una búsqueda.
    ///
    /// El doble siempre las completaba, así que «falló» y «se canceló» no se
    /// podían escribir como test — que es exactamente por lo que la ventana
    /// pintaba las tres igual («N hallazgos») sin que nada se quejara.
    pub desenlace_de_busqueda: Option<norte_proto::TaskState>,
    /// La búsqueda ni siquiera se ENCOLA, y con este error.
    ///
    /// Es otro camino que el anterior: ahí hay Task y su progreso trae el
    /// desenlace; aquí no hay Task, así que no hay progreso que lo traiga —
    /// y sin este mando ese camino no se podía escribir como test, que es
    /// por lo que la vista se quedaba diciendo «buscando…» para siempre.
    pub error_de_busqueda: Option<Error>,
    /// Cuántas veces se han enumerado los volúmenes.
    ///
    /// Lo cuenta para poder anclar un test NEGATIVO: «el diálogo no dice
    /// nada» sigue verde si nadie preguntó, y entonces no prueba que callar
    /// sea la respuesta — solo que no hubo pregunta.
    pub volumenes_pedidos: std::sync::atomic::AtomicU64,
    /// Los cambios de gobierno pedidos, en orden (`approval:id:true`…).
    pub gobierno: std::sync::Mutex<Vec<String>>,
    /// Con qué falla un cambio de gobierno, si falla.
    pub error_al_gobernar: std::sync::Mutex<Option<Error>>,
    /// Las claves escritas, en orden: `(plugin, clave, valor)`.
    pub escrituras: std::sync::Mutex<Vec<(String, String, String)>>,
    /// Con qué falla `plugin.set_config`, si falla.
    pub error_al_escribir: std::sync::Mutex<Option<Error>>,
    /// Los comandos ejecutados, en orden: `(plugin, comando)`.
    pub ejecutados: std::sync::Mutex<Vec<(String, String)>>,
    /// Qué contesta `plugin.run_command`. `None` = la salida vacía, que NO
    /// es un error: un comando puede no imprimir nada.
    pub salida_de_comando: std::sync::Mutex<Option<Result<String, Error>>>,
    /// Con qué falla `sync.apply`, si falla.
    pub error_al_aplicar: std::sync::Mutex<Option<Error>>,
    /// Los ids de task a los que se les pidió parar, en orden.
    pub canceladas_por_id: Arc<std::sync::Mutex<Vec<u64>>>,
    /// Los hashes con los que se pidió aplicar, en orden.
    pub aplicados: std::sync::Mutex<Vec<norte_proto::methods::PlanHash>>,
    /// Los planes que se pidieron: `(origen, destino, modo)`.
    pub planes_pedidos: std::sync::Mutex<Vec<(VPath, VPath, norte_proto::methods::SyncMode)>>,
    /// Las filas que contesta `fs.compare`, en un solo lote. `None` = el
    /// método falla con `Unsupported`.
    pub filas_comparadas: std::sync::Mutex<Option<Vec<norte_proto::methods::CompareRow>>>,
    /// Las comparaciones que se pidieron: `(izquierda, derecha)`.
    pub comparaciones: std::sync::Mutex<Vec<(VPath, VPath)>>,
    /// Lo que contesta `index.search_semantic`. `None` = `NotFound` (no hay
    /// índice), que es el caso que hay que saber leer.
    pub semanticos: std::sync::Mutex<Option<Vec<norte_proto::methods::SemanticHit>>>,
    /// Las consultas semánticas que se pidieron, con su `k`.
    pub semanticas_pedidas: std::sync::Mutex<Vec<(String, u32)>>,
    /// El informe que contesta `policy.undo_report`. `None` = `Unsupported`.
    pub informe_undo: std::sync::Mutex<Option<norte_proto::methods::PolicyUndoReportResult>>,
    /// Los ids de task cuyo informe de undo se pidió, en orden.
    pub informes_undo_pedidos: std::sync::Mutex<Vec<u64>>,
    /// El informe que contesta `archive.pack_report` (#250). `None` =
    /// `Unsupported`, que es lo que contesta un daemon N-1.
    pub informe_pack: std::sync::Mutex<Option<norte_proto::methods::ArchivePackReportResult>>,
    /// Los ids cuyo informe de empaquetado se pidió, en orden.
    pub informes_pack_pedidos: std::sync::Mutex<Vec<u64>>,
    /// Los ids cuya ficha se pidió, en orden.
    pub fichas_pedidas: std::sync::Mutex<Vec<String>>,
    /// Los ids que se pidieron a `plugin.help`, en orden: es lo que permite
    /// comprobar que una página se pide UNA vez y que un id inválido jamás
    /// llega al wire.
    pub paginas_pedidas: std::sync::Mutex<Vec<String>>,
    /// Los canales de la conexión, para que el test empuje eventos y tasks
    /// ajenas como haría un daemon.
    pub eventos:
        std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>>>,
    pub ajenas: std::sync::Mutex<Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>>>,
    /// El canal de `connection.degraded`, para que el test empuje uno.
    pub degradadas: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>>,
    >,
    /// El PRIMER `list` sobre un directorio que existe falla con esto (#327),
    /// y se consume: el reintento tras entregar el secreto entra.
    pub pide_secreto: std::sync::Mutex<Option<Error>>,
    /// Lo entregado por `provide_secret` (#327): `(conn, secreto)`, en orden.
    ///
    /// El secreto se guarda EN CLARO aquí a propósito: es lo que el test tiene
    /// que poder comprobar —que llega tal cual y a la conexión que lo pidió—,
    /// y este doble solo vive dentro de un test.
    pub secretos_dados: std::sync::Mutex<Vec<(String, String)>>,
    /// Qué contesta `provide_secret`. `None` = lo acepta.
    pub secreto: std::sync::Mutex<Option<Result<(), Error>>>,
    /// El canal de `connection.failed` (#322), para que el test empuje uno.
    /// Aparte del de arriba, como en el backend de verdad.
    pub fallidas: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>>,
    >,
    /// El canal de `plugin.notice` (ADR 0100), para que el test empuje uno.
    pub avisos_plugin: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>>,
    >,
    /// Los directorios que se pidió crear.
    pub creados: std::sync::Mutex<Vec<VPath>>,
    /// Qué encuentra un `stat` sobre algo que este falso CREÓ (#303).
    ///
    /// `EntryKind::File` —el defecto— es la vida normal: el fichero que el
    /// daemon acaba de poner sigue ahí y sigue siendo un fichero. Ponerlo a
    /// `Symlink` es el ataque entero: entre crear el nombre y abrirlo, alguien
    /// con permiso de escritura en ese directorio lo desenlaza y deja un
    /// enlace con el mismo nombre. El árbol de `pon` no vale para esto: lo que
    /// se crea no está en él, y quien lo comprueba pregunta por la ruta
    /// creada.
    pub creado_aparece_como: Option<EntryKind>,
    /// Los lotes de permisos que se pidieron: rutas y modo (#314).
    pub permisos: std::sync::Mutex<Vec<(Vec<VPath>, u32)>>,
    /// Las rutas de cada lote de sumas que se pidió (#311).
    pub sumas_pedidas: std::sync::Mutex<Vec<Vec<VPath>>>,
    /// Los ids de task cuyo INFORME de sumas se pidió, en orden.
    ///
    /// Existe para poder esperar a que el informe haya vuelto: un test que
    /// afirma que un informe a medias NO abre nada tiene que haberlo tenido
    /// en la mano, o estaría comprobando que todavía no ha llegado.
    pub sumas_informes_pedidos: std::sync::Mutex<Vec<u64>>,
    /// El informe que devuelve `checksum_report`. Por defecto, vacío y
    /// completo — un test que quiera digests lo pone.
    pub sumas_informe: std::sync::Mutex<norte_proto::methods::FsChecksumReportResult>,
    /// El catálogo de atributos que devuelve el falso daemon.
    pub catalogo: std::sync::Mutex<norte_proto::AttrCatalog>,
    /// Con qué error falla `policy.decide`. `None` = la decisión llega.
    pub error_al_decidir: std::sync::Mutex<Option<Error>>,
    /// El canal de aprobaciones, para que el test empuje una.
    pub aprobaciones: std::sync::Mutex<
        Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>>,
    >,
    /// Las decisiones que se mandaron: `(id, aprobada)`.
    pub decisiones: std::sync::Mutex<Vec<(u64, bool)>>,
    /// Lo que se pidió transferir, en orden:
    /// `(origen, destino, mover, política de colisión)`.
    ///
    /// La política se apunta porque es el ÚNICO parámetro que separa «la task
    /// falla» de «el fichero del destino desaparece»: sin clavarla, cambiarla
    /// a `Overwrite` dejaría toda la suite verde.
    pub transferencias: std::sync::Mutex<Vec<(VPath, VPath, bool, norte_proto::CollisionPolicy)>>,
    /// El estado en que NACE la task de una transferencia. `Running` (el
    /// default) deja que el test la mueva; `Failed` es la colisión que
    /// devuelve un daemon con `on_collision = Fail`.
    pub estado_transferencia: Option<norte_proto::TaskState>,
    /// Ids que ya repartió una transferencia: dos copias son dos tasks, y
    /// devolver el mismo id las fundiría en una fila del tablero.
    pub siguiente_task: AtomicUsize,
    /// ENCOLAR una transferencia falla con este error: un provider de solo
    /// lectura, un scope que no llega. No es lo mismo que una task que falla
    /// —esto pasa antes de que haya task— y la pantalla tiene que
    /// distinguirlo.
    pub transferencia_rechazada: Option<Error>,
    /// Lo que `log.tail` entrega en la SIGUIENTE vuelta: `(líneas, next)`
    /// (#328). Se sirve UNA vez y se vacía.
    ///
    /// Se vacía porque un anillo de verdad no vuelve a entregar lo que ya dio:
    /// un doble que repitiera haría que un sondeo de más duplicara líneas en
    /// el panel, y entonces un test que cuenta apariciones estaría midiendo el
    /// reloj en vez del encadenado del cursor.
    pub registro_remoto: std::sync::Mutex<Option<(Vec<norte_proto::methods::LogLine>, u64)>>,
    /// El `next` de la última respuesta servida. `None` = este daemon NO tiene
    /// registro que servir y contesta `Unsupported`, que es lo que hace uno de
    /// la misma versión compilado sin la feature `logging` — el único caso de
    /// degradación alcanzable, porque uno más viejo muere en el `initialize`.
    pub registro_next: std::sync::Mutex<Option<u64>>,
    /// Los cursores con los que se pidió `log.tail`, en orden. Es lo que
    /// permite comprobar que la primera vuelta manda `None` («lo que haya») y
    /// las siguientes encadenan.
    pub cursores_de_registro: std::sync::Mutex<Vec<Option<u64>>>,
    /// El nivel que el daemon dice tener puesto: lo contestan TANTO
    /// `log.level` como cada `log.tail`, igual que el daemon de verdad.
    /// `None` = no sabe de registro y `log.level` contesta `Unsupported`.
    pub nivel_remoto: std::sync::Mutex<Option<String>>,
    /// Los niveles que se le pidieron al daemon, en orden.
    pub niveles_pedidos: std::sync::Mutex<Vec<String>>,
    /// Retiene la respuesta de `log.tail` hasta que el test la suelta.
    ///
    /// Es la única forma de estar DENTRO de la ventana en la que una petición
    /// sigue volando mientras el panel se cierra y se vuelve a abrir, que es
    /// donde vive la pregunta de si una respuesta rancia puede colarse en el
    /// panel nuevo. Un `sleep` valdría de casualidad; esto no depende del
    /// reloj.
    pub puerta_registro: Option<Arc<Puerta>>,
}

impl Falso {
    /// Un directorio con ficheros sueltos.
    pub fn con(nombres: &[&'static str]) -> Arc<Self> {
        let mut f = Self::default();
        f.pon(
            "mem:///casa",
            nombres.iter().map(|n| (n.as_bytes().to_vec(), false)),
        );
        Arc::new(f)
    }

    pub fn pon(&mut self, dir: &str, entradas: impl IntoIterator<Item = (Vec<u8>, bool)>) {
        self.arbol
            .insert(dir.to_owned(), entradas.into_iter().collect());
    }

    /// El KIND exacto de una entrada, cuando «directorio o fichero» no basta.
    ///
    /// `pon` solo distingue esas dos cosas, que es lo que casi todo test
    /// necesita. Un SYMLINK es otra: `Enter` sobre él no significa lo mismo
    /// que sobre un fichero, y sin poder fabricar uno esa divergencia entre
    /// frontends no se podía escribir como test.
    pub fn pon_kind(&mut self, wire: &str, kind: EntryKind) {
        self.kinds.insert(wire.to_owned(), kind);
    }

    /// Arma la SIGUIENTE respuesta de `log.tail` (#328).
    ///
    /// A partir de aquí el doble sabe de registro: las vueltas posteriores a
    /// ésta contestan sin líneas nuevas y con el mismo `next`, que es lo que
    /// hace un anillo al que ya se le vació la cola.
    pub fn responde_log_tail(&self, lineas: Vec<norte_proto::methods::LogLine>, next: u64) {
        *self.registro_remoto.lock().expect("registro") = Some((lineas, next));
    }

    /// Este daemon no tiene registro que servir: los dos métodos contestan
    /// `Unsupported`. Es el estado por defecto, escrito para que el test que
    /// lo prueba lo DIGA en vez de depender de un `Default`.
    pub fn log_tail_no_soportado(&self) {
        *self.registro_remoto.lock().expect("registro") = None;
        *self.registro_next.lock().expect("next") = None;
    }

    /// El nivel que el daemon dice tener puesto, en `log.level` y en cada
    /// `log.tail`. Los dos contestan lo mismo, como el daemon de verdad: el
    /// nivel es UNO y global al proceso.
    pub fn log_level_contesta(&self, nivel: &str) {
        *self.nivel_remoto.lock().expect("nivel") = Some(nivel.to_owned());
    }

    /// Los niveles que se le pidieron al daemon, en orden.
    pub fn log_level_pedidos(&self) -> Vec<String> {
        self.niveles_pedidos.lock().expect("niveles").clone()
    }

    /// Los cursores con los que se pidió `log.tail`, en orden.
    pub fn cursores_pedidos(&self) -> Vec<Option<u64>> {
        self.cursores_de_registro.lock().expect("cursores").clone()
    }

    /// El doble acaba de anotar algo: quien esperaba, que mire.
    ///
    /// Va DESPUÉS de la anotación, siempre. Avisar antes despertaría a un
    /// test que volvería a ver el estado viejo y a dormirse, y esa carrera
    /// es exactamente la que este mecanismo existe para quitar.
    pub fn latido(&self) {
        self.pulso.notify_waiters();
    }

    /// Espera a que el doble haya anotado lo que se le pregunta. Sin reloj.
    ///
    /// `que` mira el doble y devuelve `Some` cuando ya está: el valor sale
    /// clonado, porque el `MutexGuard` no puede cruzar un `await`.
    ///
    /// El plazo de socorro NO es una espera: es el presupuesto de FALLO. En
    /// el camino verde no se consume ni un milisegundo —el aviso llega y la
    /// función vuelve—, y cuando se agota el test dice QUÉ esperaba en vez
    /// de reventar veinte líneas más abajo en una aserción que no explica
    /// nada. Bajo carga tampoco se vuelve frágil: quince segundos son tres
    /// órdenes de magnitud más de lo que tarda un `spawn` en correr.
    pub async fn hasta<T>(&self, que_esperaba: &str, que: impl Fn(&Self) -> Option<T>) -> T {
        const SOCORRO: std::time::Duration = std::time::Duration::from_secs(15);
        let espera = async {
            loop {
                if let Some(v) = que(self) {
                    return v;
                }
                // El futuro se arma ANTES de la segunda comprobación:
                // armarlo después perdería un latido caído justo en medio.
                let avisado = self.pulso.notified();
                if let Some(v) = que(self) {
                    return v;
                }
                avisado.await;
            }
        };
        let Ok(v) = tokio::time::timeout(SOCORRO, espera).await else {
            panic!("el doble nunca anotó: {que_esperaba}")
        };
        v
    }

    /// El cuerpo compartido de copiar y mover en el falso: apunta lo que se
    /// pidió y devuelve una Task con id PROPIO.
    /// Una task de archivo (empaquetar o comprobar) con su propio id, para que
    /// dos gestos seguidos no se pisen el canal de progreso.
    fn task_de_archivo(
        &self,
        kind: norte_proto::TaskKind,
        id: u64,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(id),
            kind,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(id),
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }

    fn transferir(
        &self,
        from: VPath,
        to: VPath,
        mover: bool,
        on_collision: norte_proto::CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        if let Some(e) = self.transferencia_rechazada.clone() {
            return Box::pin(async move { Err(e) });
        }
        self.transferencias
            .lock()
            .expect("transferencias")
            .push((from, to, mover, on_collision));
        self.latido();
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(100 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: if mover {
                norte_proto::TaskKind::Move
            } else {
                norte_proto::TaskKind::Copy
            },
            state: self
                .estado_transferencia
                .clone()
                .unwrap_or(norte_proto::TaskState::Running),
            bytes_done: 0,
            bytes_total: Some(10),
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }

    pub fn listados(&self) -> usize {
        self.listados.load(Ordering::SeqCst)
    }

    /// Cuántas lecturas ya VOLVIERON (listados, sondeos y contenidos).
    pub fn servidos(&self) -> usize {
        self.servidos.load(Ordering::SeqCst)
    }

    /// Cuántas lecturas se PIDIERON.
    pub fn pedidos(&self) -> usize {
        self.pedidos.load(Ordering::SeqCst)
    }

    /// ¿No vuela ninguna lectura? Todo lo que se pidió, ya volvió.
    pub fn en_calma(&self) -> bool {
        self.servidos() >= self.pedidos()
    }

    /// Las entradas de un directorio, tal como las devolvería el listado.
    pub fn entradas_de(&self, dir: &VPath) -> Vec<Entry> {
        let mut out: Vec<Entry> = self
            .arbol
            .get(&dir.to_wire())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(nombre, es_dir)| {
                let path = dir.join(norte_proto::Segment::new(nombre).expect("segmento"));
                let kind = self
                    .kinds
                    .get(&path.to_wire())
                    .copied()
                    .unwrap_or(if es_dir {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    });
                Entry {
                    kind,
                    path,
                    // Un directorio no tiene tamaño, como en la vida real: es
                    // lo que hace que la AUSENCIA de celda se pueda probar.
                    size: if es_dir || self.lazy { None } else { Some(1) },
                    mtime_ms: None,
                    attrs: std::collections::BTreeMap::new(),
                }
            })
            .collect();
        norte_frontend::sort_entries(&mut out);
        out
    }
}

/// El mismo path con el último segmento en mayúsculas.
fn otra_ortografia(path: &VPath) -> VPath {
    let Some(nombre) = path.file_name() else {
        return path.clone();
    };
    let gritado: Vec<u8> = nombre.as_bytes().to_ascii_uppercase();
    let Some(padre) = path.parent() else {
        return path.clone();
    };
    match norte_proto::Segment::new(gritado) {
        Ok(seg) => padre.join(seg),
        Err(_) => path.clone(),
    }
}

/// El árbol que usan los escenarios de paridad: un directorio con dos
/// subdirectorios y un nombre hostil.
pub fn arbol_de_prueba() -> Falso {
    let mut f = Falso::default();
    f.pon(
        "mem:///casa",
        vec![
            (b"docs".to_vec(), true),
            (b"fotos".to_vec(), true),
            (b"notas.txt".to_vec(), false),
            (vec![0x63, 0x61, 0x66, 0xC3, 0x28], false),
            // Un COMPRIMIDO y un ENLACE, que son las dos entradas sobre las
            // que `Enter` significa algo distinto de «es un fichero, no
            // pasa nada». El arnés de paridad no podía tocar la divergencia
            // número uno del inventario porque este árbol solo tenía
            // directorios y ficheros; su propia cabecera lo decía y apuntaba
            // a que hacía falta que el doble supiera de kinds. Ya lo sabe.
            (b"cosas.zip".to_vec(), false),
            (b"atajo".to_vec(), false),
        ],
    );
    f.pon(
        "mem:///casa/docs",
        vec![(b"a.md".to_vec(), false), (b"b.md".to_vec(), false)],
    );
    f.pon("mem:///casa/fotos", vec![(b"gato.png".to_vec(), false)]);
    // El enlace apunta a un directorio que SÍ se lista: un enlace a un
    // fichero no se resuelve —el `cd` falla y se absorbe—, y eso es otro
    // caso, no el que este árbol tiene que poder describir.
    f.pon_kind("mem:///casa/atajo", EntryKind::Symlink);
    f.pon("mem:///casa/atajo", vec![(b"dentro.md".to_vec(), false)]);
    // Y la raíz virtual del contenedor, que es a donde compone `Enter`.
    f.pon(
        "zip+mem:///casa/cosas.zip!/",
        vec![(b"leeme.txt".to_vec(), false)],
    );
    f
}

impl HostBackend for Falso {
    fn capabilities(
        &self,
        path: VPath,
    ) -> BoxFuture<'static, Result<norte_proto::Capabilities, Error>> {
        if self.error_de_capacidades {
            return Box::pin(async move { Err(Error::ProviderUnavailable { retryable: true }) });
        }
        // Por UBICACIÓN, no por provider: se busca el directorio exacto y, si
        // no está, su padre — que es lo que hace un mount de verdad.
        let caps = self
            .capacidades
            .get(&path.to_wire())
            .or_else(|| {
                path.parent()
                    .and_then(|p| self.capacidades.get(&p.to_wire()))
            })
            .copied()
            // Sin mando: lo que dice un ext4 corriente —distingue la caja— que
            // es el suelo honesto para un doble que corre en Linux.
            .unwrap_or(norte_proto::Capabilities {
                flags: norte_proto::CapabilityFlags::CASE_SENSITIVE,
                max_path: None,
            });
        Box::pin(async move { Ok(caps) })
    }

    fn plugin_list(
        &self,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginListResult, Error>> {
        self.catalogos_pedidos
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.latido();
        let plugins = self.plugins.lock().expect("plugins").clone();
        let errores = self
            .errores_de_carga
            .iter()
            .map(|(dir, reason)| norte_proto::methods::PluginLoadError {
                dir: dir.clone(),
                reason: reason.clone(),
                dir_bytes: self.bytes_de_carga.get(dir).cloned(),
            })
            .collect();
        Box::pin(async move {
            Ok(norte_proto::methods::PluginListResult {
                plugins,
                errors: errores,
            })
        })
    }

    fn plugin_help(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginHelpResult, Error>> {
        self.paginas_pedidas
            .lock()
            .expect("mutex de páginas")
            .push(id.clone());
        self.latido();
        let markdown = self.paginas.get(&id).cloned().unwrap_or_default();
        Box::pin(async move {
            Ok(norte_proto::methods::PluginHelpResult {
                markdown,
                truncated: false,
                lossy: false,
            })
        })
    }

    fn search(
        &self,
        params: norte_proto::methods::FsSearchParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                norte_ui_host::backend::HostTask,
                tokio::sync::mpsc::Receiver<norte_proto::methods::SearchHits>,
            ),
            Error,
        >,
    > {
        let patron = params.name_glob.clone().unwrap_or_default();
        self.busquedas
            .lock()
            .expect("mutex de búsquedas")
            .push(patron.clone());
        self.latido();
        if let Some(e) = self.error_de_busqueda.clone() {
            return Box::pin(async move { Err(e) });
        }
        let hallazgos = self.hallazgos.get(&patron).cloned().unwrap_or_default();
        let desenlace = self
            .desenlace_de_busqueda
            .clone()
            .unwrap_or(norte_proto::TaskState::Completed);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            let id = norte_proto::TaskId::new(77);
            let (tx, rx) = tokio::sync::mpsc::channel(8);
            let (ptx, prx) = tokio::sync::watch::channel(norte_proto::TaskProgress {
                task_id: id,
                kind: norte_proto::TaskKind::Search,
                state: norte_proto::TaskState::Running,
                bytes_done: 0,
                bytes_total: None,
                entries_done: 0,
                entries_total: None,
                current: None,
                unreadable: None,
                unvisited: None,
            });
            tokio::spawn(async move {
                let entradas: Vec<norte_proto::Entry> = hallazgos
                    .into_iter()
                    .map(|path| norte_proto::Entry {
                        path,
                        kind: norte_proto::EntryKind::File,
                        size: Some(1),
                        mtime_ms: Some(0),
                        attrs: std::collections::BTreeMap::new(),
                    })
                    .collect();
                // Un lote VACÍO no se manda: `norte-core` corta antes
                // (`if batch.is_empty() { return FlushOutcome::Continue }`),
                // y un doble que sí lo mande esconde todo lo que dependa de
                // que el primer lote llegue. Es la divergencia que tapó que
                // una búsqueda sin hallazgos no se cancelaba nunca.
                if !entradas.is_empty() {
                    let _ = tx
                        .send(norte_proto::methods::SearchHits {
                            task_id: id,
                            entries: entradas,
                            matches: None,
                        })
                        .await;
                }
                // Y termina: la vista deja de decir «buscando…». CON su
                // desenlace, que no es cosmética — «terminó», «la pararon» y
                // «se rompió» dicen tres cosas distintas sobre el disco.
                let _ = ptx.send(norte_proto::TaskProgress {
                    task_id: id,
                    kind: norte_proto::TaskKind::Search,
                    state: desenlace,
                    bytes_done: 0,
                    bytes_total: None,
                    entries_done: 1,
                    entries_total: Some(1),
                    current: None,
                    unreadable: None,
                    unvisited: None,
                });
                // El emisor vive lo que la task: soltarlo cierra el canal y
                // eso ES el final de la búsqueda.
                std::mem::forget(ptx);
            });
            Ok((
                norte_ui_host::backend::HostTask {
                    id,
                    progress: prx,
                    cancel: Arc::new(move || {
                        cancelaciones.fetch_add(1, Ordering::SeqCst);
                    }),
                    foreign: false,
                },
                rx,
            ))
        })
    }

    fn plugin_preview_styled(
        &self,
        path: VPath,
        columns: Option<u32>,
    ) -> BoxFuture<'static, Result<Option<norte_proto::methods::PluginPreviewStyled>, Error>> {
        self.anchos_de_preview
            .lock()
            .expect("mutex de anchos")
            .push(columns);
        let p = self.previews.get(&path.to_wire()).cloned();
        Box::pin(async move { Ok(p) })
    }

    fn plugin_thumbnail(
        &self,
        path: VPath,
        _max_edge: u32,
    ) -> BoxFuture<'static, Result<Option<norte_proto::methods::PluginThumbnail>, Error>> {
        let t = self.thumbnails.get(&path.to_wire()).cloned();
        Box::pin(async move { Ok(t) })
    }

    fn plugin_decorate(
        &self,
        paths: Vec<VPath>,
        kinds: Vec<norte_proto::EntryKind>,
    ) -> BoxFuture<'static, Result<Vec<norte_proto::methods::PluginDecorations>, Error>> {
        self.decorados
            .lock()
            .expect("mutex de decorados")
            .push(paths.clone());
        self.clases_decoradas
            .lock()
            .expect("mutex de clases")
            .push(kinds);
        self.latido();
        let tabla = self.decoraciones.clone();
        let iconos = self.iconos.clone();
        // Si el catálogo conoce a `acme.git` y está APAGADO, no decora: es lo
        // que hace el daemon de verdad, y lo que permite comprobar que apagar
        // un plugin desde el gestor quita sus insignias de las filas. Un
        // catálogo que no lo nombra decora como siempre.
        let git_apagado = self
            .plugins
            .lock()
            .expect("plugins")
            .iter()
            .any(|p| p.id == "acme.git" && !p.enabled);
        Box::pin(async move {
            let mut out = Vec::new();
            // Sin decoradores consentidos: «ninguna», que es lo que
            // contesta el daemon de verdad. NO una lista de vacíos.
            if !tabla.is_empty() && !git_apagado {
                out.push(norte_proto::methods::PluginDecorations {
                    plugin_id: "acme.git".to_owned(),
                    slot: norte_proto::methods::DecorationSlot::Badge,
                    decorations: paths
                        .iter()
                        .map(|p| {
                            let d = tabla.get(&p.to_wire()).cloned();
                            norte_proto::methods::DecorationWire {
                                badge: d.clone(),
                                role: d.map(|_| "warning".to_owned()),
                            }
                        })
                        .collect(),
                });
            }
            if !iconos.is_empty() {
                out.push(norte_proto::methods::PluginDecorations {
                    plugin_id: "acme.icons".to_owned(),
                    slot: norte_proto::methods::DecorationSlot::Icon,
                    decorations: paths
                        .iter()
                        .map(|p| norte_proto::methods::DecorationWire {
                            badge: iconos.get(&p.to_wire()).cloned(),
                            role: None,
                        })
                        .collect(),
                });
            }
            Ok(out)
        })
    }

    fn plugin_column_values(
        &self,
        plugin: String,
        column: String,
        paths: Vec<VPath>,
    ) -> BoxFuture<'static, Result<Vec<Option<String>>, Error>> {
        self.columnas_pedidas
            .lock()
            .expect("mutex de columnas")
            .push((plugin, column.clone(), paths.clone()));
        self.latido();
        let tabla = self.valores_de_columna.clone();
        Box::pin(async move {
            // Posicional 1:1 con `paths`, SIEMPRE: es el contrato, y un
            // vector corto es la forma de romperlo sin que se note.
            Ok(paths
                .iter()
                .map(|p| tabla.get(&(column.clone(), p.to_wire())).cloned())
                .collect())
        })
    }

    fn plugin_panel_render(
        &self,
        params: norte_proto::methods::PluginPanelRenderParams,
    ) -> BoxFuture<'static, Result<Option<norte_proto::methods::PanelFrame>, Error>> {
        self.paneles_pedidos
            .lock()
            .expect("mutex de paneles")
            .push(params);
        self.latido();
        let marco = self.marco_de_panel.clone();
        Box::pin(async move { Ok(marco) })
    }

    fn volumes(&self) -> BoxFuture<'static, Result<Vec<norte_proto::methods::Volume>, Error>> {
        self.volumenes_pedidos
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let vols = self.volumenes.clone();
        Box::pin(async move { Ok(vols) })
    }

    fn plugin_config(
        &self,
        id: String,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PluginGetConfigResult, Error>> {
        self.fichas_pedidas
            .lock()
            .expect("mutex de fichas")
            .push(id.clone());
        self.latido();
        let keys = self.esquemas.get(&id).cloned().unwrap_or_default();
        Box::pin(async move { Ok(norte_proto::methods::PluginGetConfigResult { keys }) })
    }

    fn undo_session(&self, session: String) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.deshechas.lock().expect("deshechas").push(session);
        self.latido();
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(900 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Undo,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn plugin_set_approval(
        &self,
        id: String,
        approved: bool,
        expected_digest: Option<String>,
    ) -> BoxFuture<'static, Result<(), Error>> {
        // El ancla se APUNTA (#282): que la ventana la mande es lo que un test
        // puede afirmar desde aquí, y sin apuntarla el hilo entero sería una
        // cadena de firmas sin nadie que las lea.
        self.gobierno.lock().expect("gobierno").push(format!(
            "approval:{id}:{approved}:{}",
            expected_digest.as_deref().unwrap_or("-")
        ));
        self.latido();
        let fallo = self.error_al_gobernar.lock().expect("gobierno").clone();
        // Y el catálogo cambia: el host lo REPIDE tras un OK, así que un
        // falso que contestara siempre lo mismo dejaría pasar una pantalla
        // que dice «aprobada» sin que nadie lo confirmara.
        if fallo.is_none() {
            for p in self.plugins.lock().expect("plugins").iter_mut() {
                if p.id == id {
                    p.approved = approved;
                }
            }
        }
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn plugin_set_enabled(
        &self,
        id: String,
        enabled: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.gobierno
            .lock()
            .expect("gobierno")
            .push(format!("enabled:{id}:{enabled}"));
        self.latido();
        let fallo = self.error_al_gobernar.lock().expect("gobierno").clone();
        if fallo.is_none() {
            for p in self.plugins.lock().expect("plugins").iter_mut() {
                if p.id == id {
                    p.enabled = enabled;
                }
            }
        }
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn plugin_uninstall(&self, id: String) -> BoxFuture<'static, Result<bool, Error>> {
        self.gobierno
            .lock()
            .expect("gobierno")
            .push(format!("uninstall:{id}"));
        self.latido();
        let fallo = self.error_al_gobernar.lock().expect("gobierno").clone();
        let mut tenia = false;
        if fallo.is_none() {
            // Y desaparece del catálogo: el host lo REPIDE tras un OK, y una
            // fila que siguiera ahí sería la pantalla enseñando lo borrado.
            let mut plugins = self.plugins.lock().expect("plugins");
            tenia = plugins.iter().any(|p| p.id == id && p.approved);
            plugins.retain(|p| p.id != id);
        }
        Box::pin(async move { fallo.map_or(Ok(tenia), Err) })
    }

    fn plugin_set_config(
        &self,
        id: String,
        key: String,
        value: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.escrituras
            .lock()
            .expect("escrituras")
            .push((id, key, value));
        self.latido();
        let fallo = self.error_al_escribir.lock().expect("escribir").clone();
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn plugin_run_command(
        &self,
        id: String,
        command: String,
        _arg: String,
    ) -> BoxFuture<'static, Result<String, Error>> {
        self.ejecutados
            .lock()
            .expect("ejecutados")
            .push((id, command));
        self.latido();
        let salida = self.salida_de_comando.lock().expect("salida").clone();
        Box::pin(async move { salida.unwrap_or_else(|| Ok(String::new())) })
    }

    fn read(
        &self,
        path: VPath,
        range: Option<norte_proto::ByteRange>,
    ) -> BoxFuture<'static, Result<Vec<u8>, Error>> {
        let bytes = self.contenido.get(&path.to_wire()).cloned();
        let retraso = self.retraso_ms;
        self.pedidos.fetch_add(1, Ordering::SeqCst);
        let servidos = Arc::clone(&self.servidos);
        let pulso = Arc::clone(&self.pulso);
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            servidos.fetch_add(1, Ordering::SeqCst);
            pulso.notify_waiters();
            let mut b = bytes.ok_or(Error::NotFound)?;
            if let Some(r) = range {
                let off = usize::try_from(r.offset).unwrap_or(usize::MAX).min(b.len());
                b = b.split_off(off);
                if let Some(len) = r.len {
                    b.truncate(usize::try_from(len).unwrap_or(usize::MAX));
                }
            }
            Ok(b)
        })
    }

    fn stat(&self, path: VPath, _attrs: Vec<String>) -> BoxFuture<'static, Result<Entry, Error>> {
        self.sondeos.lock().expect("sondeos").push(path.clone());
        self.pedidos.fetch_add(1, Ordering::SeqCst);
        self.latido();
        let grita = self.stat_grita;
        let retraso = self.retraso_ms;
        // El padre del path dice en qué directorio buscarlo; la entrada sale
        // del mismo árbol, pero AHORA con tamaño: es lo que hace un `stat`.
        let entrada = path.parent().and_then(|dir| {
            self.arbol.get(&dir.to_wire()).and_then(|entradas| {
                entradas
                    .iter()
                    .find(|(n, _)| path.file_name().is_some_and(|f| f.as_bytes() == n))
                    .map(|(_, es_dir)| Entry {
                        // Un provider puede contestar con OTRA ortografía del
                        // mismo nombre; el host tiene que hidratar la entrada
                        // que pidió, no la que le devuelven.
                        path: if grita {
                            otra_ortografia(&path)
                        } else {
                            path.clone()
                        },
                        kind: if *es_dir {
                            EntryKind::Dir
                        } else {
                            EntryKind::File
                        },
                        size: if *es_dir { None } else { Some(1) },
                        mtime_ms: Some(1_700_000_000_000),
                        attrs: std::collections::BTreeMap::new(),
                    })
            })
        });
        // Lo que este falso CREÓ existe, aunque no esté en el árbol de `pon`:
        // el árbol es el listado de antes de crear nada (#303).
        let entrada = entrada.or_else(|| {
            let creado = self
                .creados
                .lock()
                .expect("creados")
                .iter()
                .any(|c| c.to_wire() == path.to_wire());
            creado.then(|| Entry {
                path: path.clone(),
                kind: self.creado_aparece_como.unwrap_or(EntryKind::File),
                size: Some(0),
                mtime_ms: Some(1_700_000_000_000),
                attrs: std::collections::BTreeMap::new(),
            })
        });
        let servidos = Arc::clone(&self.servidos);
        let pulso = Arc::clone(&self.pulso);
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            servidos.fetch_add(1, Ordering::SeqCst);
            pulso.notify_waiters();
            entrada.ok_or(Error::NotFound)
        })
    }

    fn attr_catalog(
        &self,
        _dir: VPath,
    ) -> BoxFuture<'static, Result<norte_proto::AttrCatalog, Error>> {
        let c = self.catalogo.lock().expect("catálogo").clone();
        Box::pin(async move { Ok(c) })
    }

    fn take_approvals(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PolicyApprovalRequired>>
    {
        self.aprobaciones.lock().expect("aprobaciones").take()
    }

    fn policy_decide(
        &self,
        approval_id: u64,
        approve: bool,
    ) -> BoxFuture<'static, Result<(), Error>> {
        self.decisiones
            .lock()
            .expect("decisiones")
            .push((approval_id, approve));
        self.latido();
        let fallo = self
            .error_al_decidir
            .lock()
            .expect("error al decidir")
            .clone();
        Box::pin(async move { fallo.map_or(Ok(()), Err) })
    }

    fn take_conn_events(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_client::ConnEvent>> {
        self.eventos.lock().expect("eventos").take()
    }

    fn take_foreign_tasks(&self) -> Option<tokio::sync::mpsc::UnboundedReceiver<HostTask>> {
        self.ajenas.lock().expect("ajenas").take()
    }

    fn sync_apply(
        &self,
        plan_hash: norte_proto::methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.aplicados.lock().expect("aplicados").push(plan_hash);
        self.latido();
        if let Some(e) = self
            .error_al_aplicar
            .lock()
            .expect("error al aplicar")
            .clone()
        {
            return Box::pin(async move { Err(e) });
        }
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(500 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Sync,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        // Cancelable DE VERDAD: con un cancelador que no cuenta, un test de
        // cancelación pasa igual con el panel congelado.
        let canceladas = Arc::clone(&self.canceladas_por_id);
        let pulso = Arc::clone(&self.pulso);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(move || {
                    canceladas.lock().expect("canceladas").push(id.get());
                    pulso.notify_waiters();
                }),
                foreign: false,
            })
        })
    }

    fn sync_report(
        &self,
        _task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::SyncReportResult, Error>> {
        let informe = self.informe_de_sync.lock().expect("informe").clone();
        Box::pin(async move { informe.ok_or(Error::Unsupported) })
    }

    fn sync_plan(
        &self,
        params: norte_proto::methods::SyncPlanParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_client::SyncPlanEvent>,
            ),
            Error,
        >,
    > {
        self.planes_pedidos
            .lock()
            .expect("planes")
            .push((params.source, params.dest, params.mode));
        self.latido();
        let plan = self.plan_de_sync.lock().expect("plan").clone();
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(400 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::SyncPlan,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            let (pasos, mut done) = plan.ok_or(Error::Unsupported)?;
            // El cierre lleva SU Task: el modelo compartido descarta el de
            // otro plan por este id, que es justo lo que tiene que hacer.
            done.task_id = id;
            let (etx, erx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = etx
                    .send(norte_client::SyncPlanEvent::Steps(
                        norte_proto::methods::SyncStepsBatch {
                            task_id: id,
                            steps: pasos,
                        },
                    ))
                    .await;
                let _ = etx.send(norte_client::SyncPlanEvent::Done(done)).await;
            });
            Ok((
                HostTask {
                    id,
                    progress: rx,
                    cancel: Arc::new(|| {}),
                    foreign: false,
                },
                erx,
            ))
        })
    }

    fn compare(
        &self,
        params: norte_proto::methods::FsCompareParams,
    ) -> BoxFuture<
        'static,
        Result<
            (
                HostTask,
                tokio::sync::mpsc::Receiver<norte_proto::methods::CompareRowsBatch>,
            ),
            Error,
        >,
    > {
        self.comparaciones
            .lock()
            .expect("comparaciones")
            .push((params.left, params.right));
        self.latido();
        let filas = self.filas_comparadas.lock().expect("filas").clone();
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(300 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::Compare,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            let filas = filas.ok_or(Error::Unsupported)?;
            let (ftx, frx) = tokio::sync::mpsc::channel(4);
            tokio::spawn(async move {
                let _ = ftx
                    .send(norte_proto::methods::CompareRowsBatch {
                        task_id: id,
                        rows: filas,
                    })
                    .await;
            });
            Ok((
                HostTask {
                    id,
                    progress: rx,
                    cancel: Arc::new(|| {}),
                    foreign: false,
                },
                frx,
            ))
        })
    }

    fn semantic_search(
        &self,
        query: String,
        k: u32,
    ) -> BoxFuture<'static, Result<Vec<norte_proto::methods::SemanticHit>, Error>> {
        self.semanticas_pedidas
            .lock()
            .expect("semánticas")
            .push((query, k));
        self.latido();
        let hits = self.semanticos.lock().expect("semánticos").clone();
        Box::pin(async move { hits.ok_or(Error::NotFound) })
    }

    fn take_degraded(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionDegraded>>
    {
        self.degradadas.lock().expect("degradadas").take()
    }

    fn take_failed(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::ConnectionFailed>> {
        self.fallidas.lock().expect("fallidas").take()
    }

    fn take_plugin_notices(
        &self,
    ) -> Option<tokio::sync::mpsc::UnboundedReceiver<norte_proto::methods::PluginNotice>> {
        self.avisos_plugin.lock().expect("avisos_plugin").take()
    }

    /// #311: apunta el lote de sumas y devuelve una Task ya terminada. El
    /// informe lo sirve `checksum_report` con lo que diga `sumas_informe`.
    fn checksum(
        &self,
        params: norte_proto::methods::FsChecksumParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.sumas_pedidas
            .lock()
            .expect("sumas")
            .push(params.paths.clone());
        self.latido();
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(10),
            kind: norte_proto::TaskKind::Checksum,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: params.paths.len() as u64,
            entries_total: Some(params.paths.len() as u64),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progreso);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(10),
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn dir_usage(
        &self,
        params: norte_proto::methods::FsDirUsageParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.latido();
        // Ya terminada: el host pide el informe en cuanto la Task es terminal,
        // así que un doble que la deje corriendo no llegaría nunca a aterrizar
        // nada y el test mediría un silencio.
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(11),
            kind: norte_proto::TaskKind::DirUsage,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: Some(params.path),
            unreadable: None,
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progreso);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(11),
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn dir_usage_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsDirUsageReportResult, Error>> {
        let _ = task;
        self.latido();
        // Un mapa vacío pero LISTADO: el hueco se pinta sin rectángulos y sin
        // decir que mide, que es lo que un directorio vacío produce de verdad.
        Box::pin(async move {
            Ok(norte_proto::methods::FsDirUsageReportResult {
                listed: true,
                ..norte_proto::methods::FsDirUsageReportResult::default()
            })
        })
    }

    fn checksum_report(
        &self,
        task: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsChecksumReportResult, Error>> {
        self.sumas_informes_pedidos
            .lock()
            .expect("informes de sumas")
            .push(task.get());
        self.latido();
        let informe = self.sumas_informe.lock().expect("informe").clone();
        Box::pin(async move { Ok(informe) })
    }

    /// #314: apunta el lote de permisos que se pidió, para que un test pueda
    /// afirmar QUÉ rutas y con QUÉ modo — que es lo único que el host decide;
    /// el resto lo decide el core.
    fn set_mode(
        &self,
        params: norte_proto::methods::FsSetModeParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.permisos
            .lock()
            .expect("permisos")
            .push((params.paths.clone(), params.mode));
        self.latido();
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(9),
            kind: norte_proto::TaskKind::SetMode,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: params.paths.len() as u64,
            entries_total: Some(params.paths.len() as u64),
            current: None,
            unreadable: Some(0),
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progreso);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(9),
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn mkdir(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.creados.lock().expect("creados").push(path);
        self.latido();
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(8),
            kind: norte_proto::TaskKind::Mkdir,
            state: norte_proto::TaskState::Completed,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 1,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (_tx, rx) = tokio::sync::watch::channel(progreso);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(8),
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn create_file(&self, path: VPath) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.creados.lock().expect("creados").push(path);
        self.latido();
        // Con id PROPIO: el gesto de «editar uno nuevo» mira el desenlace de
        // SU task para abrir el fichero, y compartir el 8 con `mkdir` haría
        // que un test de crear directorio disparase esa apertura.
        //
        // Y nace CORRIENDO, con su terminal por detrás. Las otras del falso
        // nacen ya terminadas con el emisor caído, y eso no es lo que hace un
        // backend de verdad: el host bombea los CAMBIOS del canal, así que un
        // canal muerto no le entrega jamás un desenlace. Lo que aquí hace
        // falta es justo el desenlace.
        let vivo = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(14),
            kind: norte_proto::TaskKind::Create,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(vivo.clone());
        // El emisor se queda vivo mientras viva el doble. Soltarlo tras el
        // envío cierra el canal antes de que el host haya leído el cambio, y
        // esa es justo la carrera que este falso existe para no tener; antes
        // se compraba durmiendo cincuenta milisegundos, que es una apuesta
        // sobre cuándo bombea el host.
        *self.progreso_create.lock().expect("progreso create") = Some(tx.clone());
        tokio::spawn(async move {
            let _ = tx.send(norte_proto::TaskProgress {
                state: norte_proto::TaskState::Completed,
                entries_done: 1,
                ..vivo
            });
        });
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(14),
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn list(
        &self,
        dir: VPath,
        attrs: Vec<String>,
    ) -> BoxFuture<'static, Result<(norte_client::EntryStream, Option<u64>), Error>> {
        self.attrs_pedidos.lock().expect("attrs").push(attrs);
        self.listados.fetch_add(1, Ordering::SeqCst);
        self.latido();
        // #327: la conexión pide su contraseña. Se consume UNA vez —quien la
        // entrega vuelve a listar y esta vez tiene que entrar—, que es
        // exactamente el flujo que hay que poder probar.
        if let Some(fallo) = self
            .pide_secreto
            .lock()
            .expect("pide_secreto")
            .take()
            .filter(|_| self.arbol.contains_key(&dir.to_wire()))
        {
            return Box::pin(async move { Err(fallo) });
        }
        if !self.arbol.contains_key(&dir.to_wire()) {
            return Box::pin(async { Err(Error::NotFound) });
        }
        // Se cuenta DESPUÉS del `NotFound`: el que no vuela no se espera.
        self.pedidos.fetch_add(1, Ordering::SeqCst);
        let lazy = self.lazy;
        // El directorio bajo el que el provider cuelga sus entradas. Con
        // `padre_distinto`, OTRA ortografía del mismo sitio.
        let padre = if self.padre_distinto {
            match dir.file_name() {
                Some(seg) => dir.parent().unwrap_or_else(|| dir.clone()).join(
                    norte_proto::Segment::new(seg.as_bytes().to_ascii_uppercase())
                        .expect("segmento"),
                ),
                None => dir.clone(),
            }
        } else {
            dir.clone()
        };
        let idos = self.desaparecidos.lock().expect("desaparecidos").clone();
        // Sin ordenar: ordenar es cosa de `PaneState`, y devolverlo ya
        // ordenado escondería que el host lo delega.
        let entradas: Vec<Entry> = self
            .arbol
            .get(&dir.to_wire())
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|(nombre, es_dir)| {
                let path = padre.join(norte_proto::Segment::new(nombre).expect("segmento"));
                // El kind exacto, si alguien lo fijó (`pon_kind`). Este doble
                // construye entradas en DOS sitios —aquí y en `entradas_de`—
                // y el override tiene que estar en los dos: parchear uno solo
                // deja el test mirando un listado que el host nunca ve.
                let kind = self
                    .kinds
                    .get(&path.to_wire())
                    .copied()
                    .unwrap_or(if es_dir {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    });
                Entry {
                    kind,
                    path,
                    // Un directorio no tiene tamaño, como en la vida real: es lo
                    // que hace que la AUSENCIA de celda se pueda probar. Con
                    // `lazy`, tampoco lo tiene un fichero: es el listado del
                    // provider local (#52), donde el tamaño se sondea aparte.
                    size: if es_dir || lazy { None } else { Some(1) },
                    mtime_ms: None,
                    attrs: {
                        let mut m = std::collections::BTreeMap::new();
                        // 0o100644: lo que un provider POSIX manda de verdad,
                        // y lo que sin catálogo se pintaría como «33188».
                        m.insert("posix.mode".to_owned(), norte_proto::AttrValue::Uint(33188));
                        m
                    },
                }
            })
            .filter(|e| !idos.contains(&e.path.to_wire()))
            .collect();
        let retraso = self.retraso_ms;
        let omitidas = self.omitidas;
        let puerta = self.puerta_drenaje.clone();
        let servidos = Arc::clone(&self.servidos);
        let pulso = Arc::clone(&self.pulso);
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            servidos.fetch_add(1, Ordering::SeqCst);
            pulso.notify_waiters();
            // 100 = `FIRST_PAGE` del host: la entrada 101 es la primera del
            // DRENAJE, y es ahí donde se corta.
            let stream: norte_client::EntryStream = Box::pin(futures::stream::unfold(
                (entradas.into_iter().enumerate(), puerta),
                |(mut it, puerta)| async move {
                    let (i, e) = it.next()?;
                    if i == 100
                        && let Some(p) = &puerta
                    {
                        p.esperar().await;
                    }
                    Some((Ok(e), (it, puerta)))
                },
            ));
            Ok((stream, omitidas))
        })
    }

    fn session_get(
        &self,
    ) -> BoxFuture<'static, Result<(norte_proto::methods::Session, bool), Error>> {
        let (sesion, duena) = self.sesion.lock().expect("sesión").clone();
        // Sin sesión puesta —revisión 0, lo que `Default` da— esta ventana es
        // la dueña, como en una instalación nueva: el daemon contesta
        // `owner: true` a la primera conexión aunque no haya nada guardado.
        // Un test que quiera una ventana SUELTA pone una sesión y dice `false`.
        let duena = duena || sesion.revision == 0;
        Box::pin(async move { Ok((sesion, duena)) })
    }

    fn session_put(
        &self,
        _version: u32,
        _revision: u64,
        body: serde_json::Value,
    ) -> BoxFuture<'static, Result<u64, Error>> {
        if self.conflicto {
            return Box::pin(async {
                Err(Error::Conflict {
                    conflict: norte_proto::ConflictKind::Exists,
                })
            });
        }
        // El core rehúsa el cuerpo ENTERO por tamaño (#316). El mando cuenta
        // los rechazos que le quedan, así que un test puede pedir «el primero
        // no, el segundo sí», que es la degradación con reintento.
        {
            let mut quedan = self.rechazos_por_tamano.lock().expect("rechazos");
            if *quedan > 0 {
                *quedan -= 1;
                self.puestas.lock().expect("puestas").push(body);
                self.latido();
                return Box::pin(async {
                    Err(Error::LimitExceeded {
                        limit: Error::LIMIT_SESSION_BODY.to_owned(),
                    })
                });
            }
        }
        self.puestas.lock().expect("puestas").push(body.clone());
        *self.escrito.lock().expect("escrito") = Some(body);
        self.latido();
        Box::pin(async { Ok(9) })
    }

    fn copy(
        &self,
        from: VPath,
        to: VPath,
        on_collision: norte_proto::CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.transferir(from, to, false, on_collision)
    }

    fn ai_rename_plan(
        &self,
        _dir: VPath,
        instruction: String,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiRenamePlanResult, Error>> {
        self.instrucciones
            .lock()
            .expect("instrucciones")
            .push(instruction);
        self.latido();
        // Los nombres que viajaron (#121): es lo que permite ver que un plan
        // pedido sobre cinco ficheros no manda los mil del directorio.
        self.nombres_ia.lock().expect("nombres_ia").push(names);
        self.latido();
        let plan = self.plan_ia.clone();
        let retraso = self.retraso_ia_ms;
        // Pedir un plan es una LECTURA: el modelo no muta nada. Entra en la
        // misma cuenta que los listados, que es lo que permite esperar a que
        // «no vuele ninguna» sin contar a mano las respuestas de cada caso.
        self.pedidos.fetch_add(1, Ordering::SeqCst);
        let servidos = Arc::clone(&self.servidos);
        let pulso = Arc::clone(&self.pulso);
        Box::pin(async move {
            if retraso > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(retraso)).await;
            }
            servidos.fetch_add(1, Ordering::SeqCst);
            pulso.notify_waiters();
            let Some(pares) = plan else {
                return Err(Error::Unsupported);
            };
            Ok(norte_proto::methods::AiRenamePlanResult {
                entries: pares
                    .into_iter()
                    .map(|(from, to)| norte_proto::methods::AiRenameEntry { from, to })
                    .collect(),
                refused: None,
            })
        })
    }

    fn plugin_rename_plan(
        &self,
        plugin_id: String,
        renamer_id: String,
        _dir: VPath,
        names: Vec<String>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::AiRenamePlanResult, Error>> {
        self.renamers_pedidos
            .lock()
            .expect("renamers")
            .push((plugin_id, renamer_id, names));
        self.latido();
        let plan = self.plan_renamer.clone();
        let rehusa = self.renamer_rehusa.clone();
        Box::pin(async move {
            if let Some(why) = rehusa {
                return Ok(norte_proto::methods::AiRenamePlanResult {
                    entries: Vec::new(),
                    refused: Some(why),
                });
            }
            let Some(pares) = plan else {
                return Err(Error::NotFound);
            };
            Ok(norte_proto::methods::AiRenamePlanResult {
                entries: pares
                    .into_iter()
                    .map(|(from, to)| norte_proto::methods::AiRenameEntry { from, to })
                    .collect(),
                refused: None,
            })
        })
    }

    fn rename_batch_plan(
        &self,
        _dir: VPath,
        pairs: Vec<norte_proto::methods::RenamePair>,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsRenameBatchPlanResult, Error>> {
        self.veredictos_pedidos
            .lock()
            .expect("veredictos")
            .push(pairs);
        self.latido();
        let v = self.veredicto.clone();
        Box::pin(async move { v.ok_or(Error::Unsupported) })
    }

    fn rename_batch(
        &self,
        dir: VPath,
        pairs: Vec<norte_proto::methods::RenamePair>,
        plan_hash: norte_proto::methods::PlanHash,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.lotes
            .lock()
            .expect("lotes")
            .push((dir, pairs, plan_hash));
        self.latido();
        let n = self.siguiente_task.fetch_add(1, Ordering::SeqCst);
        let id = norte_proto::TaskId::new(200 + n as u64);
        let progreso = norte_proto::TaskProgress {
            task_id: id,
            kind: norte_proto::TaskKind::RenameBatch,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx.clone());
        self.progresos
            .lock()
            .expect("progresos")
            .insert(id.get(), tx);
        Box::pin(async move {
            Ok(HostTask {
                id,
                progress: rx,
                cancel: Arc::new(|| {}),
                foreign: false,
            })
        })
    }

    fn rename_batch_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::FsRenameBatchReportResult, Error>> {
        self.informes_pedidos
            .lock()
            .expect("informes")
            .push(task_id.get());
        self.latido();
        let informe = self.informe.lock().expect("informe").clone();
        Box::pin(async move { informe.ok_or(Error::Unsupported) })
    }

    fn undo_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::PolicyUndoReportResult, Error>> {
        self.informes_undo_pedidos
            .lock()
            .expect("informes undo")
            .push(task_id.get());
        self.latido();
        let informe = self.informe_undo.lock().expect("informe undo").clone();
        Box::pin(async move { informe.ok_or(Error::Unsupported) })
    }

    fn archive_pack_report(
        &self,
        task_id: norte_proto::TaskId,
    ) -> BoxFuture<'static, Result<norte_proto::methods::ArchivePackReportResult, Error>> {
        self.informes_pack_pedidos
            .lock()
            .expect("informes pack")
            .push(task_id.get());
        self.latido();
        let informe = self.informe_pack.lock().expect("informe pack").clone();
        Box::pin(async move { informe.ok_or(Error::Unsupported) })
    }

    fn move_(
        &self,
        from: VPath,
        to: VPath,
        on_collision: norte_proto::CollisionPolicy,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.transferir(from, to, true, on_collision)
    }

    fn delete(&self, path: VPath, mode: DeleteMode) -> BoxFuture<'static, Result<HostTask, Error>> {
        if let Some(e) = self
            .error_al_borrar
            .lock()
            .expect("error al borrar")
            .clone()
        {
            self.borrados.lock().expect("borrados").push((path, mode));
            self.latido();
            return Box::pin(async move { Err(e) });
        }
        if self.borrar_de_verdad {
            self.desaparecidos
                .lock()
                .expect("desaparecidos")
                .insert(path.to_wire());
        }
        self.borrados.lock().expect("borrados").push((path, mode));
        self.latido();
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(7),
            kind: norte_proto::TaskKind::Delete,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: Some(10),
            entries_done: 0,
            entries_total: Some(1),
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(7),
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }

    fn pack(
        &self,
        params: norte_proto::methods::ArchivePackParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.empaquetados.lock().expect("empaquetados").push(params);
        self.latido();
        self.task_de_archivo(norte_proto::TaskKind::Pack, 11)
    }

    fn test_archive(
        &self,
        params: norte_proto::methods::ArchiveTestParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.comprobados.lock().expect("comprobados").push(params);
        self.latido();
        self.task_de_archivo(norte_proto::TaskKind::TestArchive, 12)
    }

    fn connections(
        &self,
    ) -> BoxFuture<'static, Result<Vec<norte_proto::methods::ConnectionEntry>, Error>> {
        let cs = self
            .conexiones
            .lock()
            .expect("conexiones")
            .clone()
            .unwrap_or_else(|| Ok(Vec::new()));
        Box::pin(async move { cs })
    }

    fn provide_secret(
        &self,
        conn: String,
        secret: String,
    ) -> BoxFuture<'static, Result<(), Error>> {
        // Se apunta lo entregado para que el test compruebe que llega TAL
        // CUAL: el punto entero de #327 es que la contraseña no la toca nadie
        // entre el campo y el core.
        self.secretos_dados
            .lock()
            .expect("secretos_dados")
            .push((conn, secret));
        self.latido();
        let res = self
            .secreto
            .lock()
            .expect("secreto")
            .clone()
            .unwrap_or(Ok(()));
        Box::pin(async move { res })
    }

    fn close_connection(&self, path: VPath) -> BoxFuture<'static, Result<bool, Error>> {
        self.cerradas.lock().expect("cerradas").push(path);
        self.latido();
        let res = self
            .cierre
            .lock()
            .expect("cierre")
            .clone()
            .unwrap_or(Ok(true));
        Box::pin(async move { res })
    }

    fn split_file(
        &self,
        params: norte_proto::methods::FileSplitParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.partidos.lock().expect("partidos").push(params);
        self.latido();
        self.task_de_archivo(norte_proto::TaskKind::Split, 13)
    }

    fn combine_files(
        &self,
        params: norte_proto::methods::FileCombineParams,
    ) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.juntados.lock().expect("juntados").push(params);
        self.latido();
        self.task_de_archivo(norte_proto::TaskKind::Combine, 14)
    }

    fn log_tail(
        &self,
        cursor: Option<u64>,
        _max: u32,
    ) -> BoxFuture<'static, Result<norte_proto::methods::LogTailResult, Error>> {
        self.cursores_de_registro
            .lock()
            .expect("cursores")
            .push(cursor);
        self.latido();
        // La respuesta se resuelve AQUÍ, no dentro del futuro: lo que el test
        // arma es lo que estaba puesto cuando la petición SALIÓ, y con la
        // puerta echada hay dos peticiones vivas a la vez.
        let armado = self.registro_remoto.lock().expect("registro").take();
        let nivel = self
            .nivel_remoto
            .lock()
            .expect("nivel")
            .clone()
            .unwrap_or_else(|| "info".to_owned());
        let next = {
            let mut ultimo = self.registro_next.lock().expect("next");
            if let Some((_, n)) = &armado {
                *ultimo = Some(*n);
            }
            *ultimo
        };
        let puerta = self.puerta_registro.clone();
        Box::pin(async move {
            if let Some(p) = puerta {
                p.esperar().await;
            }
            // Sin `next` no se ha servido nada nunca: este daemon no tiene
            // anillo que servir.
            let Some(next) = next else {
                return Err(Error::Unsupported);
            };
            Ok(norte_proto::methods::LogTailResult {
                lines: armado.map(|(l, _)| l).unwrap_or_default(),
                next,
                lost: 0,
                level: nivel,
                capacity: 64,
            })
        })
    }

    fn log_level(&self, level: String) -> BoxFuture<'static, Result<String, Error>> {
        self.niveles_pedidos
            .lock()
            .expect("niveles")
            .push(level.clone());
        self.latido();
        // Lo que contesta es lo que el daemon TIENE puesto, no lo que se pidió:
        // su anillo nunca baja de nivel, así que pedir menos verbosidad deja el
        // que ya había.
        let nivel = self.nivel_remoto.lock().expect("nivel").clone();
        Box::pin(async move { nivel.ok_or(Error::Unsupported) })
    }

    fn dir_size(&self, paths: Vec<VPath>) -> BoxFuture<'static, Result<HostTask, Error>> {
        self.recuentos.lock().expect("recuentos").push(paths);
        self.latido();
        let progreso = norte_proto::TaskProgress {
            task_id: norte_proto::TaskId::new(9),
            kind: norte_proto::TaskKind::DirSize,
            state: norte_proto::TaskState::Running,
            bytes_done: 0,
            bytes_total: None,
            entries_done: 0,
            entries_total: None,
            current: None,
            unreadable: None,
            unvisited: None,
        };
        let (tx, rx) = tokio::sync::watch::channel(progreso);
        *self.progreso.lock().expect("progreso") = Some(tx);
        let cancelaciones = Arc::clone(&self.cancelaciones);
        Box::pin(async move {
            Ok(HostTask {
                id: norte_proto::TaskId::new(9),
                progress: rx,
                cancel: Arc::new(move || {
                    cancelaciones.fetch_add(1, Ordering::SeqCst);
                }),
                foreign: false,
            })
        })
    }
}
