//! uid/gid → nombre de usuario y de grupo (ADR 0145), para las columnas
//! `posix.owner` y `posix.group`.
//!
//! Se pregunta a la libc (`getpwuid_r`/`getgrgid_r`) y no a `/etc/passwd`:
//! solo la libc ve lo que NSS resuelve —LDAP, SSSD, systemd-homed—, y un
//! nombre que falta ahí es el que falta en `ls -l`. Esa pregunta puede ir a
//! la red, así que:
//!
//! - se llama SOLO desde código que ya es bloqueante (el `stat` de un listado
//!   corre en `spawn_blocking`), jamás desde un contexto async;
//! - la respuesta se guarda [`VIGENCIA`] por id, para que un directorio de
//!   diez mil ficheros del mismo dueño pregunte una vez y no diez mil;
//! - cada pregunta corre en su propio hilo y se espera [`PLAZO`]: un servidor
//!   de directorio colgado deja la celda en blanco en vez de colgar el
//!   listado, que no se puede cancelar mientras está DENTRO de la libc. Tras
//!   un plazo vencido no se pregunta por ids nuevos durante [`VIGENCIA`]
//!   (el «freno»), así que un NSS muerto cuesta un plazo por minuto y no uno
//!   por dueño;
//! - la caché NO retiene el candado mientras pregunta: a lo sumo dos
//!   listados preguntan lo mismo a la vez.
//!
//! El nombre se devuelve en BYTES: POSIX no obliga a que sea UTF-8, y el que
//! lo pinta ya sabe enmascarar bytes ajenos.

use std::collections::HashMap;
use std::ffi::CStr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

/// Cuánto vale una respuesta, y cuánto dura el freno tras un plazo vencido:
/// lo bastante para que un listado grande pregunte una vez por dueño, y lo
/// bastante poco para que un usuario recién creado o renombrado aparezca sin
/// reiniciar el daemon.
const VIGENCIA: Duration = Duration::from_mins(1);

/// Lo que se espera a NSS. Un `/etc/passwd` local contesta en microsegundos
/// y un LDAP sano en milisegundos; 200 ms no recorta ninguna respuesta real,
/// como `CAPS_AT_DEADLINE` en el provider.
const PLAZO: Duration = Duration::from_millis(200);

/// Ids distintos que se recuerdan. Pasado el tope se vacía entera: es una
/// caché, no un registro. Un montaje con más dueños que esto vuelve a
/// preguntar por todos tras vaciarse, que es más lento pero no incorrecto.
const TOPE: usize = 4096;

/// Tope del búfer de la libc. Una entrada de `passwd` con este tamaño no
/// existe; pasar de aquí es un NSS roto, y se contesta «sin nombre».
const BUF_MAX: usize = 1 << 20;

/// Reintentos ante `EINTR`: una señal a mitad de la pregunta no es una
/// respuesta, y no debe acabar guardada como «no tiene nombre».
const REINTENTOS_EINTR: usize = 3;

/// Lo que contesta la libc, distinguiendo lo que se puede recordar de lo que
/// no.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Respuesta {
    /// El id tiene este nombre.
    Nombre(Vec<u8>),
    /// La libc contestó, y el id no tiene nombre: se recuerda.
    SinNombre,
    /// La libc NO contestó (error, señal, búfer imposible): no se recuerda,
    /// para que la próxima vez se vuelva a preguntar.
    Fallo,
}

type Resolver = fn(u32) -> Respuesta;

/// Por id: cuándo se supo y qué se supo (`None` = no tiene nombre).
type Mapa = HashMap<u32, (Instant, Option<Vec<u8>>)>;

/// Caché y freno de un tipo de id.
struct Cache {
    mapa: Mutex<Mapa>,
    /// Hasta cuándo no se pregunta por ids nuevos (un plazo venció).
    freno: Mutex<Option<Instant>>,
}

impl Cache {
    fn new() -> Self {
        Self {
            mapa: Mutex::new(HashMap::new()),
            freno: Mutex::new(None),
        }
    }

    fn guardar(&self, id: u32, cuando: Instant, valor: Option<Vec<u8>>) {
        // Un candado envenenado solo deja de cachear.
        if let Ok(mut m) = self.mapa.lock() {
            if m.len() >= TOPE && !m.contains_key(&id) {
                m.clear();
            }
            m.insert(id, (cuando, valor));
        }
    }
}

static USUARIOS: LazyLock<Cache> = LazyLock::new(Cache::new);
static GRUPOS: LazyLock<Cache> = LazyLock::new(Cache::new);

/// El nombre del usuario `uid`, o `None` si el sistema no le conoce ninguno
/// o no contestó a tiempo.
pub(crate) fn usuario(uid: u32) -> Option<Vec<u8>> {
    consultar(&USUARIOS, uid, Instant::now(), PLAZO, resolver_usuario)
}

/// El nombre del grupo `gid`, o `None` si el sistema no le conoce ninguno o
/// no contestó a tiempo.
pub(crate) fn grupo(gid: u32) -> Option<Vec<u8>> {
    consultar(&GRUPOS, gid, Instant::now(), PLAZO, resolver_grupo)
}

/// La caché, el freno y el plazo. `ahora`, `plazo` y `resolver` vienen de
/// fuera para probarlos sin dormir y sin depender de los usuarios de la
/// máquina.
fn consultar(
    cache: &'static Cache,
    id: u32,
    ahora: Instant,
    plazo: Duration,
    resolver: Resolver,
) -> Option<Vec<u8>> {
    if let Ok(m) = cache.mapa.lock()
        && let Some((cuando, valor)) = m.get(&id)
        && ahora.saturating_duration_since(*cuando) < VIGENCIA
    {
        return valor.clone();
    }
    if let Ok(f) = cache.freno.lock()
        && f.is_some_and(|hasta| ahora < hasta)
    {
        return None;
    }
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let lanzado = std::thread::Builder::new()
        .name("norte-nss".to_owned())
        .spawn(move || {
            let r = resolver(id);
            // El hilo guarda lo que sepa AUNQUE quien preguntó ya no espere:
            // una respuesta que llega tarde sirve al siguiente listado.
            match &r {
                Respuesta::Nombre(n) => cache.guardar(id, ahora, Some(n.clone())),
                Respuesta::SinNombre => cache.guardar(id, ahora, None),
                Respuesta::Fallo => {}
            }
            let _ = tx.send(r);
        });
    if lanzado.is_err() {
        // Sin hilos no hay pregunta acotada; mejor en blanco que sin plazo.
        return None;
    }
    match rx.recv_timeout(plazo) {
        Ok(Respuesta::Nombre(n)) => Some(n),
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            if let Ok(mut f) = cache.freno.lock() {
                *f = Some(ahora + VIGENCIA);
            }
            None
        }
        Ok(Respuesta::SinNombre | Respuesta::Fallo)
        | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => None,
    }
}

/// `getpwuid_r` con un búfer que crece mientras la libc diga `ERANGE`.
#[allow(unsafe_code)]
fn resolver_usuario(uid: u32) -> Respuesta {
    let mut buf: Vec<libc::c_char> = vec![0; 1024];
    let mut interrupciones = 0;
    loop {
        // SAFETY: `libc::passwd` es un POD de punteros y enteros: todo ceros
        // (punteros nulos) es un valor válido, y la libc lo rellena antes de
        // que se lea.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut res: *mut libc::passwd = std::ptr::null_mut();
        // SAFETY: `pwd`, `res` y `buf` son propios y viven hasta el final de
        // la iteración; se pasa la longitud REAL de `buf`, y `getpwuid_r` es
        // reentrante: solo escribe en ellos.
        let rc = unsafe {
            libc::getpwuid_r(uid, &raw mut pwd, buf.as_mut_ptr(), buf.len(), &raw mut res)
        };
        if rc == libc::ERANGE && buf.len() < BUF_MAX {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc == libc::EINTR && interrupciones < REINTENTOS_EINTR {
            interrupciones += 1;
            continue;
        }
        if rc != 0 {
            return Respuesta::Fallo;
        }
        if res.is_null() || pwd.pw_name.is_null() {
            return Respuesta::SinNombre;
        }
        // SAFETY: con `rc == 0` y `res` no nulo, `pw_name` apunta a una cadena
        // terminada en NUL DENTRO de `buf`, que sigue vivo; se copia antes de
        // soltarlo.
        let nombre = unsafe { CStr::from_ptr(pwd.pw_name) };
        return Respuesta::Nombre(nombre.to_bytes().to_vec());
    }
}

/// `getgrgid_r`, igual que [`resolver_usuario`].
#[allow(unsafe_code)]
fn resolver_grupo(gid: u32) -> Respuesta {
    let mut buf: Vec<libc::c_char> = vec![0; 1024];
    let mut interrupciones = 0;
    loop {
        // SAFETY: `libc::group` es un POD de punteros y enteros: todo ceros es
        // un valor válido, y la libc lo rellena antes de que se lea.
        let mut grp: libc::group = unsafe { std::mem::zeroed() };
        let mut res: *mut libc::group = std::ptr::null_mut();
        // SAFETY: `grp`, `res` y `buf` son propios y viven hasta el final de
        // la iteración; se pasa la longitud REAL de `buf`, y `getgrgid_r` es
        // reentrante: solo escribe en ellos.
        let rc = unsafe {
            libc::getgrgid_r(gid, &raw mut grp, buf.as_mut_ptr(), buf.len(), &raw mut res)
        };
        if rc == libc::ERANGE && buf.len() < BUF_MAX {
            buf.resize(buf.len() * 2, 0);
            continue;
        }
        if rc == libc::EINTR && interrupciones < REINTENTOS_EINTR {
            interrupciones += 1;
            continue;
        }
        if rc != 0 {
            return Respuesta::Fallo;
        }
        if res.is_null() || grp.gr_name.is_null() {
            return Respuesta::SinNombre;
        }
        // SAFETY: con `rc == 0` y `res` no nulo, `gr_name` apunta a una cadena
        // terminada en NUL DENTRO de `buf`, que sigue vivo; se copia antes de
        // soltarlo.
        let nombre = unsafe { CStr::from_ptr(grp.gr_name) };
        return Respuesta::Nombre(nombre.to_bytes().to_vec());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    /// Una caché propia por test: las globales se comparten entre hilos de
    /// test y ensuciarían la cuenta.
    fn cache() -> &'static Cache {
        Box::leak(Box::new(Cache::new()))
    }

    /// El uid 0 tiene nombre en todo unix; qué nombre no se fija (un
    /// contenedor puede llamarlo distinto), solo que llega sin el NUL de C.
    #[test]
    fn el_uid_cero_tiene_nombre_y_sin_nul() {
        let Respuesta::Nombre(n) = resolver_usuario(0) else {
            panic!("uid 0 con nombre");
        };
        assert!(!n.is_empty());
        assert!(!n.contains(&0), "el NUL de C no se cuela: {n:?}");
    }

    #[test]
    fn el_gid_cero_tiene_nombre_y_sin_nul() {
        let Respuesta::Nombre(n) = resolver_grupo(0) else {
            panic!("gid 0 con nombre");
        };
        assert!(!n.is_empty());
        assert!(!n.contains(&0), "el NUL de C no se cuela: {n:?}");
    }

    /// Un id que nadie tiene es «sin nombre» —que se recuerda—, no un fallo
    /// ni un pánico.
    #[test]
    fn un_id_sin_dueno_es_sin_nombre() {
        assert_eq!(resolver_usuario(u32::MAX - 7), Respuesta::SinNombre);
        assert_eq!(resolver_grupo(u32::MAX - 7), Respuesta::SinNombre);
    }

    /// Dentro de la vigencia no se vuelve a preguntar; pasada, sí.
    #[test]
    fn la_cache_respeta_la_vigencia() {
        static PREGUNTAS: AtomicUsize = AtomicUsize::new(0);
        fn ana(_: u32) -> Respuesta {
            PREGUNTAS.fetch_add(1, Ordering::SeqCst);
            Respuesta::Nombre(b"ana".to_vec())
        }
        let c = cache();
        let t0 = Instant::now();
        let plazo = Duration::from_secs(10);
        assert_eq!(consultar(c, 7, t0, plazo, ana), Some(b"ana".to_vec()));
        assert_eq!(
            consultar(c, 7, t0 + VIGENCIA / 2, plazo, ana),
            Some(b"ana".to_vec())
        );
        assert_eq!(PREGUNTAS.load(Ordering::SeqCst), 1, "de la caché");
        let _ = consultar(c, 7, t0 + VIGENCIA, plazo, ana);
        assert_eq!(PREGUNTAS.load(Ordering::SeqCst), 2, "pasada, se pregunta");
    }

    /// Un «no tiene nombre» también se recuerda: un uid huérfano repetido
    /// en diez mil ficheros no son diez mil viajes a NSS.
    #[test]
    fn la_cache_recuerda_tambien_la_ausencia() {
        static PREGUNTAS: AtomicUsize = AtomicUsize::new(0);
        fn nadie(_: u32) -> Respuesta {
            PREGUNTAS.fetch_add(1, Ordering::SeqCst);
            Respuesta::SinNombre
        }
        let c = cache();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(consultar(c, 9, t0, Duration::from_secs(10), nadie), None);
        }
        assert_eq!(PREGUNTAS.load(Ordering::SeqCst), 1);
    }

    /// Un FALLO (una señal, un NSS que devolvió error) no se recuerda: la
    /// siguiente vez se vuelve a preguntar, en vez de dejar un dueño real en
    /// blanco durante un minuto.
    #[test]
    fn un_fallo_no_se_recuerda() {
        static PREGUNTAS: AtomicUsize = AtomicUsize::new(0);
        fn falla(_: u32) -> Respuesta {
            PREGUNTAS.fetch_add(1, Ordering::SeqCst);
            Respuesta::Fallo
        }
        let c = cache();
        let t0 = Instant::now();
        for _ in 0..3 {
            assert_eq!(consultar(c, 5, t0, Duration::from_secs(10), falla), None);
        }
        assert_eq!(PREGUNTAS.load(Ordering::SeqCst), 3);
    }

    /// Un NSS colgado deja la celda en blanco tras el plazo, y durante la
    /// vigencia no se le vuelve a preguntar por ids nuevos. La pregunta se
    /// bloquea en un candado que el test suelta al final: nada de dormir.
    #[test]
    fn un_nss_colgado_vence_el_plazo_y_frena() {
        static PREGUNTAS: AtomicUsize = AtomicUsize::new(0);
        static PUERTA: Mutex<()> = Mutex::new(());
        fn colgado(_: u32) -> Respuesta {
            PREGUNTAS.fetch_add(1, Ordering::SeqCst);
            let _abierta = PUERTA.lock();
            Respuesta::Nombre(b"tarde".to_vec())
        }
        let cerrada = PUERTA.lock().expect("puerta");
        let c = cache();
        let t0 = Instant::now();
        let plazo = Duration::from_millis(1);
        assert_eq!(consultar(c, 1, t0, plazo, colgado), None, "vence el plazo");
        // El hilo colgado cuenta su pregunta cuando arranca, que puede ser
        // después de vencer el plazo: se espera a ESA condición, no a un reloj.
        let limite = Instant::now() + Duration::from_secs(30);
        while PREGUNTAS.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < limite, "el hilo de NSS nunca arrancó");
            std::thread::yield_now();
        }
        assert_eq!(consultar(c, 2, t0, plazo, colgado), None, "frenado");
        assert_eq!(
            PREGUNTAS.load(Ordering::SeqCst),
            1,
            "con el freno puesto no se lanza otra pregunta"
        );
        drop(cerrada);
        // Pasado el freno se vuelve a preguntar (y ya contesta).
        let tras = t0 + VIGENCIA;
        assert_eq!(
            consultar(c, 2, tras, Duration::from_secs(10), colgado),
            Some(b"tarde".to_vec())
        );
    }

    /// Pasado el tope, la caché se vacía en vez de crecer sin fin.
    #[test]
    fn la_cache_no_crece_sin_tope() {
        let c = cache();
        let t0 = Instant::now();
        for id in 0..=u32::try_from(TOPE).expect("cabe") {
            c.guardar(id, t0, None);
        }
        assert!(c.mapa.lock().expect("candado").len() <= TOPE);
    }
}
