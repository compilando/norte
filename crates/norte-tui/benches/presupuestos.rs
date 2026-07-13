//! Benchmarks de los presupuestos de la spec §12 (fase 10):
//! - arranque frío (config + keymaps + listado + primer draw) < 50 ms
//! - listado de 100k entradas hasta primer render < 200 ms
//!
//! `just bench` los corre; criterion imprime medias — compara contra el
//! presupuesto a ojo (gates duros de tiempo en CI = flakiness).

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use norte_core::Engine;
use norte_proto::{Entry, VPath};
use norte_tui::app::{App, Pane, sort_entries};
use norte_tui::config::{Layers, load};
use norte_tui::keymap::{COMMANDS, Effective, Screen, presets};
use norte_tui::ui;
use norte_vfs_local::LocalProvider;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Drena el listado ENTERO + sort (vara de regresión del coste total).
fn listar(rt: &tokio::runtime::Runtime, engine: &Engine, dir: &VPath) -> Vec<Entry> {
    use futures::StreamExt;
    rt.block_on(async {
        let mut stream = engine.list(dir).await.expect("list");
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item.expect("entry"));
        }
        sort_entries(&mut out);
        out
    })
}

/// PRIMERA página (hasta 100) + sort: el camino real del primer render con
/// paginación (ADR 0017). No drena las 100k — es lo que #27 mide de verdad.
fn primera_pagina(rt: &tokio::runtime::Runtime, engine: &Engine, dir: &VPath) -> Vec<Entry> {
    use futures::StreamExt;
    rt.block_on(async {
        let mut stream = engine.list(dir).await.expect("list");
        let mut out = Vec::with_capacity(100);
        while out.len() < 100 {
            match stream.next().await {
                Some(item) => out.push(item.expect("entry")),
                None => break,
            }
        }
        sort_entries(&mut out);
        out
    })
}

fn draw_once(app: &App) {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    black_box(terminal.backend().to_string().len());
}

/// Arranque frío en-proceso: todo lo que pasa entre `main()` y el primer
/// frame (menos el exec del binario y la init del runtime, ~1-2 ms).
fn bench_cold_start(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    for i in 0..100 {
        std::fs::write(dir.path().join(format!("f{i:03}")), b"x").expect("write");
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    c.bench_function("cold_start_hasta_primer_frame", |b| {
        b.iter(|| {
            let cfg = load(&Layers { dirs: vec![] }).expect("config");
            let presets = presets();
            let (_, preset) = presets.iter().find(|(n, _)| *n == cfg.preset).unwrap();
            let browse =
                Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Browse).unwrap();
            let viewer =
                Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Viewer).unwrap();
            black_box((&browse, &viewer));
            let engine = Engine::new();
            engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())));
            let root = LocalProvider::root();
            let entries = listar(&rt, &engine, &root);
            let app = App::new(
                Pane::new(root.clone(), entries.clone()),
                Pane::new(root.clone(), entries),
            );
            draw_once(&app);
        });
    });
}

/// 100k entradas: listado por el engine + sort NFC + primer render.
fn bench_list_100k(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    eprintln!("creando 100k archivos (una vez)…");
    for i in 0..100_000u32 {
        std::fs::File::create(dir.path().join(format!("archivo-{i:06}.dat"))).expect("create");
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())));
    let root = LocalProvider::root();
    let mut group = c.benchmark_group("listado");
    group.sample_size(10);
    // El criterio de #27: primer render con la PRIMERA página (paginación,
    // ADR 0017). Presupuesto spec §12 <200 ms (esperado ~1-3 ms: no espera a
    // las 100k); de paso verifica el <16 ms de spec §11.
    group.bench_function("cien_mil_hasta_primer_render", |b| {
        b.iter(|| {
            let entries = primera_pagina(&rt, &engine, &root);
            let mut pane = Pane::new(root.clone(), entries);
            pane.loading = true; // el resto se rellenaría en background
            let app = App::new(pane, Pane::new(root.clone(), Vec::new()));
            draw_once(&app);
        });
    });
    // Vara de regresión del coste TOTAL (drenar+sort las 100k): la métrica de
    // la issue diferida de los statx de vfs-local (d_type/size lazy).
    group.bench_function("cien_mil_drenado_completo", |b| {
        b.iter(|| {
            let entries = listar(&rt, &engine, &root);
            let app = App::new(
                Pane::new(root.clone(), entries),
                Pane::new(root.clone(), Vec::new()),
            );
            draw_once(&app);
        });
    });
    group.finish();
}

criterion_group!(presupuestos, bench_cold_start, bench_list_100k);
criterion_main!(presupuestos);
