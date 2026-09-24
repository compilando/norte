//! Benchmarks for the spec §12 budgets (phase 10):
//! - cold start (config + keymaps + listing + first draw) < 50 ms
//! - listing of 100k entries up to first render < 200 ms
//! - Lua statusbar hook (M4 Lua T9): cached ≈ nothing (< 1 µs), uncached
//!   with a trivial script < 1 ms (the HARD budget is by instructions:
//!   50k, `lua/statusbar.rs`)
//!
//! `just bench` runs them; criterion prints averages — compare against the
//! budget by eye (hard time gates in CI = flakiness).

use std::hint::black_box;
use std::sync::Arc;

use criterion::{Criterion, criterion_group, criterion_main};
use norte_core::Engine;
use norte_frontend::PaneState;
use norte_proto::{Entry, EntryKind, VPath};
use norte_tui::app::{App, Pane};
use norte_tui::config::{Layers, load};
use norte_tui::keymap::{COMMANDS, Effective, Screen, presets};
use norte_tui::ui;
use norte_vfs_local::LocalProvider;
use ratatui::Terminal;
use ratatui::backend::TestBackend;

/// Drains the ENTIRE listing (total-cost regression yardstick). #54: does
/// NOT sort here — `Pane::new` (via `PaneState::new`) normalizes
/// internally; sorting here too would be duplicated work and would
/// misrepresent the bench.
fn list_all(rt: &tokio::runtime::Runtime, engine: &Engine, dir: &VPath) -> Vec<Entry> {
    use futures::StreamExt;
    rt.block_on(async {
        let mut stream = engine.list(dir).await.expect("list");
        let mut out = Vec::new();
        while let Some(item) = stream.next().await {
            out.push(item.expect("entry"));
        }
        out
    })
}

/// FIRST page (up to 100): the real path of the first render with
/// pagination (ADR 0017). Does not drain the 100k — this is what #27
/// actually measures. #54: does NOT sort here, same reason as [`list_all`].
fn first_page(rt: &tokio::runtime::Runtime, engine: &Engine, dir: &VPath) -> Vec<Entry> {
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
        out
    })
}

fn draw_once(app: &App) {
    let mut terminal = Terminal::new(TestBackend::new(120, 40)).expect("terminal");
    terminal.draw(|f| ui::draw(f, app)).expect("draw");
    black_box(terminal.backend().to_string().len());
}

/// In-process cold start: everything that happens between `main()` and the
/// first frame (minus the binary's exec and runtime init, ~1-2 ms).
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
    c.bench_function("cold_start_to_first_frame", |b| {
        b.iter(|| {
            let cfg = load(&Layers { dirs: vec![] }).expect("config");
            let presets = presets();
            let (_, preset) = presets
                .iter()
                .find(|(n, _)| *n == cfg.common.preset)
                .unwrap();
            let browse =
                Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Browse).unwrap();
            let viewer =
                Effective::build_for(preset, &cfg.keymap_layers, COMMANDS, Screen::Viewer).unwrap();
            black_box((&browse, &viewer));
            let engine = Engine::new();
            engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())));
            let root = LocalProvider::root();
            let entries = list_all(&rt, &engine, &root);
            let app = App::new(
                Pane::new(root.clone(), entries.clone()),
                Pane::new(root.clone(), entries),
            );
            draw_once(&app);
        });
    });
}

/// 100k entries: engine listing + NFC sort + first render.
fn bench_list_100k(c: &mut Criterion) {
    let dir = tempfile::tempdir().expect("tempdir");
    eprintln!("creating 100k files (once)…");
    for i in 0..100_000u32 {
        std::fs::File::create(dir.path().join(format!("file-{i:06}.dat"))).expect("create");
    }
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("runtime");
    let engine = Engine::new();
    engine.register_provider(Arc::new(LocalProvider::rooted(dir.path())));
    let root = LocalProvider::root();
    let mut group = c.benchmark_group("listing");
    group.sample_size(10);
    // #27's criterion: first render with the FIRST page (pagination, ADR
    // 0017). Spec §12 budget <200 ms (expected ~1-3 ms: does not wait for
    // the 100k); along the way it also verifies spec §11's <16 ms.
    group.bench_function("hundred_k_to_first_render", |b| {
        b.iter(|| {
            let entries = first_page(&rt, &engine, &root);
            let mut pane = Pane::new(root.clone(), entries);
            pane.set_loading(true); // the rest would be filled in in the background
            let app = App::new(pane, Pane::new(root.clone(), Vec::new()));
            draw_once(&app);
        });
    });
    // TOTAL-cost regression yardstick (drain+sort the 100k): the metric for
    // vfs-local's deferred statx issue (lazy d_type/size).
    group.bench_function("hundred_k_full_drain", |b| {
        b.iter(|| {
            let entries = list_all(&rt, &engine, &root);
            let app = App::new(
                Pane::new(root.clone(), entries),
                Pane::new(root.clone(), Vec::new()),
            );
            draw_once(&app);
        });
    });
    group.finish();
}

/// #54: TOTAL cost of the batched extend path (what the drain bench does
/// not capture: there it sorts ONCE at the end; the real fill used to
/// re-sort on every batch). 100k synthetic entries in batches of 4096 →
/// ~24 extends. PURE CPU (no FS, no engine): measures only
/// `PaneState::extend`.
fn bench_fill_100k(c: &mut Criterion) {
    let dir = VPath::parse("mem:///bench").expect("wire");
    let all: Vec<Entry> = (0..100_000)
        .map(|i| Entry {
            attrs: std::collections::BTreeMap::new(),
            // Mixes dirs/files and shuffled names (a worse case for the
            // merge than the FS's arrival order, which is already
            // semi-sorted).
            path: VPath::parse(&format!("mem:///bench/f{:06}", (i * 7919) % 100_000))
                .expect("wire"),
            kind: if i % 8 == 0 {
                EntryKind::Dir
            } else {
                EntryKind::File
            },
            size: None,
            mtime_ms: None,
        })
        .collect();
    let mut group = c.benchmark_group("fill");
    group.sample_size(10);
    group.bench_function("hundred_k_extend_in_batches", |b| {
        b.iter(|| {
            let mut pane = PaneState::new(dir.clone(), Vec::new());
            for chunk in all.chunks(4096) {
                pane.extend(chunk.to_vec());
            }
            black_box(pane.entries().len())
        });
    });
    group.finish();
}

/// Lua statusbar hook (M4 Lua): the CACHED call (same `StatusInput`) fires
/// on every render pass and must be negligible; the UNCACHED one (changed
/// snapshot) re-invokes the script under its instruction budget.
fn bench_lua_statusbar(c: &mut Criterion) {
    use norte_tui::lua::{Layer, LuaHost, StatusInput};

    let host = LuaHost::new().expect("lua");
    host.eval_layer(
        b"norte.ui.statusbar(function(s) return s.entries .. ' entries in ' .. s.cwd end)",
        Layer::User,
    )
    .expect("statusbar hook");
    let input = StatusInput {
        cwd: b"mem:///a/dir".to_vec(),
        selected: 3,
        selected_bytes: 4096,
        entries: 1234,
        tasks: 0,
    };
    assert!(host.statusbar(&input).is_some(), "the hook responds");

    c.bench_function("lua_statusbar_cached", |b| {
        b.iter(|| black_box(host.statusbar(black_box(&input))));
    });
    let mut n = 0usize;
    c.bench_function("lua_statusbar_uncached", |b| {
        b.iter(|| {
            // A different snapshot on every pass: busts the cache, re-invokes.
            n += 1;
            let mut i = input.clone();
            i.selected = n;
            black_box(host.statusbar(&i))
        });
    });
}

criterion_group!(
    presupuestos,
    bench_cold_start,
    bench_list_100k,
    bench_fill_100k,
    bench_lua_statusbar
);
criterion_main!(presupuestos);
