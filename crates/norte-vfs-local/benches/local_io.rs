//! El benchmark que la ADR 0002 aplazó (#12): ¿tiene `tokio-uring` algo que
//! ganar aquí?
//!
//! La ADR eligió Tokio multi-hilo con `spawn_blocking` para todo el I/O local y
//! dejó `tokio-uring` como «optimización tras un benchmark que la justifique».
//! Esto es ese benchmark, y está escrito para contestar la pregunta REAL, que
//! no es «cuánto tarda» sino **dónde se va el tiempo**:
//!
//! - si domina el trabajo del KERNEL (leer el directorio, mover los bytes),
//!   `io_uring` no lo quita: hace las mismas llamadas con otro envoltorio;
//! - si domina lo que norte pone ENCIMA —el salto al pool de bloqueo, el canal
//!   con contrapresión, construir un `Entry` por fila— `io_uring` tampoco lo
//!   quita, porque nada de eso es una llamada al sistema.
//!
//! Por eso mide las dos mitades por separado en vez de un único número: el
//! listado CRUDO (`std::fs::read_dir` a pelo, el suelo del sistema) contra el
//! listado por el provider (el mismo trabajo más todo lo de norte). La
//! DIFERENCIA es el techo de lo que cualquier cambio de motor de I/O podría
//! recortar, y se mide sin escribir una segunda implementación — que es lo que
//! el benchmark existe para decidir.
//!
//! `just bench` lo corre. Criterion imprime medias y se comparan a ojo: un gate
//! duro de tiempo en CI es intermitencia, no una medida.

use std::hint::black_box;
use std::time::Duration;

use criterion::{Criterion, criterion_group, criterion_main};
use futures::StreamExt as _;
use norte_proto::VPath;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

/// Cuántas entradas tiene el directorio grande.
///
/// La issue pedía 100 000. Se quedan en 20 000 porque el número que se busca es
/// el coste POR ENTRADA, que ya es plano mucho antes: cinco veces más ficheros
/// dan la misma respuesta y multiplican por cinco lo que tarda cada `iter` de
/// criterion —que los repite— y lo que ocupa el tempdir de quien lo corra.
const ENTRADAS: usize = 20_000;

/// Un directorio con [`ENTRADAS`] ficheros VACÍOS.
///
/// Vacíos a propósito: lo que se mide es enumerar, no leer. Un byte de
/// contenido metería el coste de abrir en la cuenta del listado.
fn dir_grande() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..ENTRADAS {
        std::fs::write(dir.path().join(format!("f{i:06}.txt")), b"").expect("write");
    }
    dir
}

fn listado(c: &mut Criterion) {
    let dir = dir_grande();
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let mut g = c.benchmark_group("listado");
    // Menos muestras que el default: cada una recorre 20 000 entradas.
    g.sample_size(20);
    g.measurement_time(Duration::from_secs(15));

    // EL SUELO: lo que cuesta el sistema, sin norte de por medio. No es una
    // alternativa que se pueda usar —no da `Entry`, ni cancelación, ni
    // contrapresión— es la referencia contra la que se lee lo de abajo.
    //
    // Llama a `file_type()` porque el provider TAMBIÉN lo llama, y en Linux
    // sale del `d_type` del dirent sin una llamada al sistema de más. Sin esta
    // línea el suelo salía más barato de lo que es y el sobrecoste medido
    // habría estado inflado a favor de la conclusión.
    g.bench_function("crudo_read_dir", |b| {
        b.iter(|| {
            let mut n = 0usize;
            for d in std::fs::read_dir(dir.path()).expect("read_dir").flatten() {
                black_box(d.file_type().expect("file_type"));
                n += 1;
            }
            black_box(n)
        });
    });

    // EL CAMINO DE VERDAD: `spawn_blocking` + canal acotado + un `Entry` por
    // fila. La diferencia con el de arriba es el techo del que hablan la ADR
    // 0002 y la issue #12.
    g.bench_function("provider_list", |b| {
        let provider = LocalProvider::rooted(dir.path().to_path_buf());
        let raiz = VPath::parse("file:///").expect("wire válido");
        b.iter(|| {
            rt.block_on(async {
                let mut s = provider.list(&raiz).await.expect("list");
                let mut n = 0usize;
                while let Some(entrada) = s.next().await {
                    // Se cuenta y se tira: lo que se mide es producirla, y
                    // acumularlas metería el coste de un `Vec` de 20 000
                    // `Entry` en la cuenta del listado.
                    black_box(&entrada.expect("entrada"));
                    n += 1;
                }
                black_box(n)
            })
        });
    });
    g.finish();
}

/// Tamaño de la copia. La issue decía 10 GiB; criterion REPITE cada medida, así
/// que diez gibibytes por iteración son decenas de minutos y un disco lleno.
/// 256 MiB da el mismo número —MB/s— porque el coste es lineal en cuanto el
/// fichero no cabe en la caché de página.
const COPIA: usize = 256 * 1024 * 1024;

fn copia(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    let origen = dir.path().join("origen.bin");
    // Contenido NO comprimible ni todo ceros: un fichero de ceros lo puede
    // resolver el sistema de ficheros sin mover un byte, y entonces esto mide
    // la creación de un hueco.
    let bloque: Vec<u8> = (0..=255u8).cycle().take(1024 * 1024).collect();
    {
        use std::io::Write as _;
        let f = std::fs::File::create(&origen).expect("create");
        let mut w = std::io::BufWriter::new(f);
        for _ in 0..(COPIA / bloque.len()) {
            w.write_all(&bloque).expect("write");
        }
        w.flush().expect("flush");
    }
    let rt = tokio::runtime::Runtime::new().expect("runtime");
    let provider = LocalProvider::rooted(dir.path().to_path_buf());
    let de = VPath::parse("file:///origen.bin").expect("wire válido");

    let mut g = c.benchmark_group("copia");
    g.sample_size(10);
    g.measurement_time(Duration::from_secs(30));
    g.throughput(criterion::Throughput::Bytes(COPIA as u64));
    // Leer por el provider y tirar los bytes: mide el camino de LECTURA, que
    // es la mitad que un motor de I/O distinto podría cambiar. Escribir
    // también metería el `fsync` del destino, que es del kernel y de nadie
    // más.
    g.bench_function("provider_read", |b| {
        b.iter(|| {
            rt.block_on(async {
                let mut s = provider.read(&de, None).await.expect("read");
                let mut n = 0u64;
                while let Some(trozo) = s.next().await {
                    n += trozo.expect("trozo").len() as u64;
                }
                black_box(n)
            })
        });
    });
    g.finish();
}

criterion_group!(benches, listado, copia);
criterion_main!(benches);
