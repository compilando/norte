//! Benchmarks del copy engine CROSS-PROVIDER (fase 10c M2). Modela un remoto
//! con `MemProvider` + latencia por-op inyectada (espejo de sftp/S3 sin red ni
//! Docker — el plan M2 usa el Mem como espejo de los remotos); el destino es un
//! `LocalProvider` real sobre un tempdir.
//!
//! `just bench` los corre; criterion imprime medias — se comparan a ojo (gates
//! duros de tiempo en CI = flakiness). No es un test: mide throughput y sirve
//! de vara de regresión del engine.

use std::hint::black_box;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use norte_core::Engine;
use norte_proto::{Segment, TaskState, VPath};
use norte_testkit::MemProvider;
use norte_vfs::Provider;
use norte_vfs_local::LocalProvider;

fn vp(wire: &str) -> VPath {
    VPath::parse(wire).expect("wire válido")
}

/// Engine con un Mem (origen "remoto", con `latency` opcional por-op) y un
/// `LocalProvider` (destino real). Devuelve también el tempdir (debe vivir).
fn setup(latency: Option<Duration>) -> (Engine, Arc<MemProvider>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let engine = Engine::new();
    let mem = Arc::new(MemProvider::new());
    mem.faults().set_latency_per_op(latency);
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    let local = LocalProvider::rooted(dir.path().to_path_buf());
    engine.register_provider(Arc::new(local) as Arc<dyn Provider>);
    (engine, mem, dir)
}

fn seed_file(rt: &tokio::runtime::Runtime, mem: &MemProvider, wire: &str, size: usize) {
    rt.block_on(async {
        let mut sink = mem.write(&vp(wire)).await.expect("write");
        sink.write(Bytes::from(vec![0xAB; size]))
            .await
            .expect("chunk");
        sink.commit().await.expect("commit");
    });
}

fn seed_tree(rt: &tokio::runtime::Runtime, mem: &MemProvider, n: usize) {
    rt.block_on(async {
        mem.mkdir(&vp("mem:///tree")).await.expect("mkdir");
        for i in 0..n {
            let p = vp("mem:///tree").join(Segment::new(format!("f{i:04}").into_bytes()).unwrap());
            let mut sink = mem.write(&p).await.expect("write");
            sink.write(Bytes::from_static(b"payload de tamano modesto"))
                .await
                .expect("chunk");
            sink.commit().await.expect("commit");
        }
    });
}

fn run_copy(rt: &tokio::runtime::Runtime, engine: &Engine, from: &str, to: &str) {
    rt.block_on(async {
        let h = engine.copy(&vp(from), &vp(to)).await.expect("submit");
        assert_eq!(h.join().await, TaskState::Completed);
    });
}

/// Un archivo grande (4 MiB): mide el throughput de streaming read+write del
/// engine, con y sin latencia de "remoto".
fn bench_large_file(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let mut g = c.benchmark_group("copy_archivo_grande_4MiB");
    for (label, latency) in [
        ("local_espejo", None),
        ("remoto_500us_op", Some(Duration::from_micros(500))),
    ] {
        g.bench_function(label, |b| {
            b.iter_batched(
                || {
                    let (engine, mem, dir) = setup(latency);
                    seed_file(&rt, &mem, "mem:///big.bin", 4 * 1024 * 1024);
                    (engine, dir)
                },
                |(engine, dir)| {
                    run_copy(&rt, &engine, "mem:///big.bin", "file:///big.bin");
                    black_box(dir);
                },
                BatchSize::SmallInput,
            );
        });
    }
    g.finish();
}

/// Un árbol de muchos archivos pequeños: mide el coste POR-ENTRADA (walk +
/// mkdir + copy), donde la latencia por-op del remoto domina.
fn bench_many_files(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let mut g = c.benchmark_group("copy_arbol_64_archivos");
    for (label, latency) in [
        ("local_espejo", None),
        ("remoto_200us_op", Some(Duration::from_micros(200))),
    ] {
        g.bench_function(label, |b| {
            b.iter_batched(
                || {
                    let (engine, mem, dir) = setup(latency);
                    seed_tree(&rt, &mem, 64);
                    (engine, dir)
                },
                |(engine, dir)| {
                    run_copy(&rt, &engine, "mem:///tree", "file:///tree");
                    black_box(dir);
                },
                BatchSize::SmallInput,
            );
        });
    }
    g.finish();
}

criterion_group!(benches, bench_large_file, bench_many_files);
criterion_main!(benches);
