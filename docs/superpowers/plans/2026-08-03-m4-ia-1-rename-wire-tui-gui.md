# M4-IA-1: AI rename over the wire + TUI + GUI — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Expose the existing `Engine::ai_rename_plan` over the wire (`ai.rename_plan`, proto 0.32.0) and give the TUI and GUI a reviewable-plan AI rename flow.

**Architecture:** New direct (non-Task) JSON-RPC method, cancellable via `rpc.cancel` (#72 mechanism, drop-based). Daemon builds the AI provider at startup (degrading, never aborting). `Backend::ai_rename_plan` bridges embedded/remote. TUI: free-text instruction modal (Tier A) → in-flight run cancellable with Esc → decision plan modal (Tier B) → N governed `fs.move`. GUI: `SessionCmd`/`SessionEvent` pair + two modal variants.

**Tech Stack:** Existing only — norte-proto/serde, norte-core engine+daemon, ratatui TUI, GPUI GUI, Fluent i18n. No new dependencies.

**Spec:** `docs/superpowers/specs/2026-08-03-m4-ia-design.md` (IA-1 half).
**Deviation from spec text:** spec sketches `AiRenamePlanParams { dir: String }`; the crate convention types wire paths as `VPath` (serializes to the same wire string — cf. `IndexQueryParams.root`). This plan uses `VPath`.

**Conventions that apply to every task:** work on `main` (project practice), Conventional Commits, `cargo nextest run -p <crate>` + `cargo clippy -p <crate> --all-targets -- -D warnings` per task; full `just ci` only in Task 7. All new user-facing strings go to BOTH `i18n/en.ftl` and `i18n/es.ftl` (parity test-enforced).

---

### Task 1: proto 0.32.0 — `ai.rename_plan` method, types, goldens, schema

**Files:**
- Modify: `crates/norte-proto/src/methods.rs` (const block ~line 420; struct block after ~line 760; `PROTOCOL_VERSION` at line 268-273)
- Modify: `crates/norte-proto/tests/golden/types/methods.json`
- Modify: `crates/norte-proto/tests/golden_types.rs` (new family fn; count pin ~line 426-440; registry pins ~line 1707-1722)
- Modify: `crates/norte-proto/tests/types.rs` (`version_ventana_actual` ~line 900)
- Modify: `crates/norte-proto/tests/schema.rs` (`ProtocolSchema` fields, alphabetical — `ai_*` before `attr_hint` at line 18)
- Regenerate: `docs/schema/proto.schema.json`

- [ ] **Step 1: Add golden fixtures (failing test first)**

In `crates/norte-proto/tests/golden/types/methods.json`, add at the top of the object (keys alphabetical; `ai_` sorts first). Params fixture uses a hostile (non-UTF-8, percent-encoded) dir per house convention; entry names are plain UTF-8 (engine guarantee):

```json
  "ai_rename_plan_params": {
    "dir": "file:///home/user/fotos-a%FF%FE",
    "instruction": "kebab-case, date first"
  },
  "ai_rename_plan_result": {
    "entries": [
      { "from": "IMG 001.jpg", "to": "2024-01-01-beach.jpg" }
    ]
  },
```

- [ ] **Step 2: Add the golden checks**

In `crates/norte-proto/tests/golden_types.rs`, next to `check_methods_index` (line 441):

```rust
/// Familia `ai.*` (0.32.0, M4-IA, ADR 0031): plan de rename revisable.
fn check_methods_ai(fixtures: &BTreeMap<String, Value>) {
    use norte_proto::VPath;
    use norte_proto::methods::{AiRenameEntry, AiRenamePlanParams, AiRenamePlanResult};
    // Dir HOSTIL (no-UTF8, percent-encoded en el wire de VPath).
    check_one(
        fixtures,
        "ai_rename_plan_params",
        &AiRenamePlanParams {
            dir: VPath::parse("file:///home/user/fotos-a%FF%FE").unwrap(),
            instruction: "kebab-case, date first".into(),
        },
    );
    check_one(
        fixtures,
        "ai_rename_plan_result",
        &AiRenamePlanResult {
            entries: vec![AiRenameEntry {
                from: "IMG 001.jpg".into(),
                to: "2024-01-01-beach.jpg".into(),
            }],
        },
    );
}
```

In `golden_methods()` (line 426): add `check_methods_ai(&fixtures);` and bump the count pin:

```rust
    // 98 → 100 en 0.32.0: + ai_rename_plan_params/result (M4-IA, ADR 0031).
    assert_eq!(fixtures.len(), 100, "[methods.json] fixtures sin caso Rust");
```

In the registry test (~line 1707-1722): add `assert_eq!(methods::AI_RENAME_PLAN, "ai.rename_plan");` and change the version pin to `assert_eq!(norte_proto::PROTOCOL_VERSION, "0.32.0");`.

- [ ] **Step 3: Run to verify failure**

Run: `cargo nextest run -p norte-proto golden_methods`
Expected: FAIL — `AI_RENAME_PLAN` / `AiRenamePlanParams` not found (compile error).

- [ ] **Step 4: Add the method const, types, and version bump**

In `crates/norte-proto/src/methods.rs`, const block (after `INDEX_QUERY`, ~line 420):

```rust
/// Sugiere un plan de rename REVISABLE para `dir` (M4-IA, ADR 0031). Respuesta
/// DIRECTA (no Task) pero CANCELABLE con `rpc.cancel` (#72): la llamada al
/// proveedor de IA puede tardar segundos. NO muta nada — aplicar el plan son N
/// [`FS_MOVE`] ordinarios (journal + undo + policy).
/// [`AiRenamePlanParams`] → [`AiRenamePlanResult`].
pub const AI_RENAME_PLAN: &str = "ai.rename_plan";
```

Struct block (next to the other params/results, e.g. after the index family):

```rust
/// Params de [`AI_RENAME_PLAN`] (M4-IA, ADR 0031).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePlanParams {
    /// Directorio cuyos basenames se envían al proveedor (tras el gate de IA).
    pub dir: VPath,
    /// Instrucción del usuario.
    pub instruction: String,
}

/// Una pareja del plan de [`AI_RENAME_PLAN`]. Nombres BASE, UTF-8 garantizado:
/// el engine rechaza nombres hostiles fail-loud ANTES de llamar al proveedor y
/// valida `to` como `Segment` (sin `/`, `..`, NUL, `!` ni `\`).
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenameEntry {
    /// Nombre existente en `dir`.
    pub from: String,
    /// Nombre destino propuesto.
    pub to: String,
}

/// Resultado de [`AI_RENAME_PLAN`]: el plan REVISABLE (spec §9). Vacío = el
/// modelo no propuso cambios. El plan es el producto: aplicarlo son N
/// [`FS_MOVE`] gobernados; este método jamás muta.
#[cfg_attr(feature = "schema", derive(schemars::JsonSchema))]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiRenamePlanResult {
    /// Parejas from→to (solo las que cambian de nombre).
    pub entries: Vec<AiRenameEntry>,
}
```

Version bump — replace the const line and append the changelog paragraph above it:

```rust
/// 0.32.0 (M4-IA, ADR 0031): método nuevo `ai.rename_plan` (aditivo —
/// [`AiRenamePlanParams`] → [`AiRenamePlanResult`], respuesta directa
/// cancelable con `rpc.cancel`). Ventana N=0.32.x / N-1=0.31.x: un cliente
/// 0.31 jamás llama al método nuevo — nada que gatear en emisión.
pub const PROTOCOL_VERSION: &str = "0.32.0";
```

- [ ] **Step 5: Shift the version-window pin**

In `crates/norte-proto/tests/types.rs` (~line 900), `version_ventana_actual`:

```rust
    // 0.32.0 (M4-IA): acepta 0.32.x (N) y 0.31.x (N-1), rechaza 0.30.x (N-2).
    assert!(version_compatible(PROTOCOL_VERSION, "0.32.9"), "N");
    assert!(version_compatible(PROTOCOL_VERSION, "0.31.0"), "N-1");
    assert!(!version_compatible(PROTOCOL_VERSION, "0.30.9"), "N-2 fuera de la ventana");
```

- [ ] **Step 6: Register in ProtocolSchema and regenerate**

In `crates/norte-proto/tests/schema.rs`, add to `struct ProtocolSchema` (alphabetical, before `attr_hint`):

```rust
    ai_rename_entry: AiRenameEntry,
    ai_rename_plan_params: AiRenamePlanParams,
    ai_rename_plan_result: AiRenamePlanResult,
```

(plus the matching `use norte_proto::methods::{AiRenameEntry, AiRenamePlanParams, AiRenamePlanResult};`).

Run: `NORTE_UPDATE_SCHEMA=1 cargo test -p norte-proto --features schema --test schema`
Expected: PASS, `docs/schema/proto.schema.json` regenerated (git diff shows the three new `$defs`).

- [ ] **Step 7: Run the full proto test suite**

Run: `cargo nextest run -p norte-proto && cargo clippy -p norte-proto --all-targets --features schema -- -D warnings && cargo test -p norte-proto --features schema --doc`
Expected: PASS (golden_methods green, count 100, version pins green).

- [ ] **Step 8: Commit**

```bash
git add crates/norte-proto docs/schema/proto.schema.json
git commit -m "feat(proto): ai.rename_plan method — proto 0.32.0 (M4-IA, ADR 0031)"
```

---

### Task 2: daemon — handler, cancelable set, startup AI wiring, tests

**Files:**
- Modify: `crates/norte-core/src/daemon/server.rs` (`dispatch_fs_task` ~line 2841; `cancelable` matches! at ~line 1565)
- Modify: `crates/norte-cli/src/main.rs` (`daemon_cmd` Run arm, insert between `set_connector` ~line 1190 and `Daemon::bind_with_policy` ~line 1191)
- Test: `crates/norte-core/tests/daemon.rs`

- [ ] **Step 1: Write the failing daemon tests**

In `crates/norte-core/tests/daemon.rs`. Copy the `FakeAi` provider from `crates/norte-core/tests/ai_rename.rs:20-50` (test binaries can't share code; copy the struct + its `AiProvider` impl verbatim, reply split into two stream deltas). Add a spawn helper forking `spawn_daemon_mem` (line 65):

```rust
/// Daemon con proveedor de IA fake instalado y `[ai]` habilitado (M4-IA).
async fn spawn_daemon_ai(reply: &str) -> TestDaemon {
    let dir = tempfile::tempdir().expect("tempdir");
    let socket = dir.path().join("d.sock");
    let engine = Arc::new(Engine::new());
    let mem = Arc::new(MemProvider::default());
    engine.register_provider(Arc::clone(&mem) as Arc<dyn Provider>);
    engine.set_ai_provider(Arc::new(FakeAi::new(reply, true)));
    engine.set_ai_config(norte_core::ai::AiConfig {
        enabled: true,
        ..Default::default()
    });
    let daemon = Daemon::bind(
        Arc::clone(&engine),
        DaemonConfig { socket_path: Some(socket.clone()), ..DaemonConfig::default() },
    )
    .await
    .expect("bind");
    let run = tokio::spawn(daemon.run());
    TestDaemon { socket, run, _dir: dir, mem }
}
```

(Adjust `FakeAi::new` to whatever constructor shape the copied struct has — `ai_rename.rs:68` `engine_with` shows the wiring; keep the `AiConfig` literal in sync with `enabled()` at `ai_rename.rs:85`.)

Tests:

```rust
#[tokio::test]
async fn ai_rename_plan_responde_por_el_socket() {
    let d = spawn_daemon_ai(r#"[{"from":"a.txt","to":"informe-a.txt"}]"#).await;
    write_file(&d.mem, "mem:///a.txt", b"x").await;
    let c = connected_client(&d).await;
    let plan: methods::AiRenamePlanResult = c
        .call(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams { dir: vp("mem:///"), instruction: "prefija informe-".into() },
        )
        .await
        .expect("ai.rename_plan");
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].from, "a.txt");
    assert_eq!(plan.entries[0].to, "informe-a.txt");
}

#[tokio::test]
async fn ai_rename_plan_sin_proveedor_es_unsupported() {
    let d = spawn_daemon(None).await; // sin set_ai_provider
    let c = connected_client(&d).await;
    let err = c
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams { dir: vp("mem:///"), instruction: "x".into() },
        )
        .await
        .expect_err("sin proveedor → error");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::Unsupported)),
            "Unsupported, fue {:?}", rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}

#[tokio::test]
async fn agente_sin_scope_no_puede_ai_rename_plan() {
    let d = spawn_daemon_policy().await;
    let agent = connected_agent(&d, "s1").await;
    let err = agent
        .call::<_, methods::AiRenamePlanResult>(
            methods::AI_RENAME_PLAN,
            &methods::AiRenamePlanParams { dir: vp("mem:///"), instruction: "x".into() },
        )
        .await
        .expect_err("agente sin scope denegado");
    match err {
        ClientError::Rpc(rpc) => assert!(
            matches!(rpc.data, Some(norte_proto::Error::PolicyDenied { ref rule }) if rule == "out-of-scope"),
            "PolicyDenied out-of-scope, fue {:?}", rpc.data
        ),
        other => panic!("esperaba Rpc, fue {other:?}"),
    }
}
```

Plus the spec-required cancel-mid-call test. Give the copied `FakeAi` an optional pre-reply delay (`delay: Option<Duration>`, `tokio::time::sleep(d).await` before yielding the first stream delta), then:

Mechanics copied from `rpc_cancel_retira_el_ask_suspendido_sin_matar_la_conexion` (daemon.rs:3387): `call_tracked` captures the JSON-RPC id, `notify(RPC_CANCEL)` retires it.

```rust
#[tokio::test]
async fn rpc_cancel_aborta_ai_rename_plan_en_vuelo() {
    // FakeAi con delay grande: la request queda EN VUELO hasta el cancel.
    let d = spawn_daemon_ai_slow(r#"[]"#, Duration::from_secs(30)).await;
    let c = Arc::new(connected_client(&d).await);

    let id_slot = Arc::new(std::sync::Mutex::new(None::<u64>));
    let caller = Arc::clone(&c);
    let slot = Arc::clone(&id_slot);
    let call = tokio::spawn(async move {
        caller
            .call_tracked::<_, methods::AiRenamePlanResult>(
                methods::AI_RENAME_PLAN,
                &methods::AiRenamePlanParams { dir: vp("mem:///"), instruction: "x".into() },
                move |id| *slot.lock().expect("id lock") = Some(id),
            )
            .await
    });

    let id = loop {
        if let Some(id) = *id_slot.lock().expect("id lock") {
            break id;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    };
    c.notify(
        methods::RPC_CANCEL,
        &methods::RpcCancelParams { id: norte_proto::wire::RequestId::Num(id) },
    )
    .expect("rpc.cancel notify");

    match call.await.expect("join") {
        Err(ClientError::Rpc(rpc)) => {
            assert_eq!(rpc.code, codes::APP_ERROR);
            assert_eq!(rpc.data, Some(Error::Cancelled));
        }
        other => panic!("esperaba Cancelled, fue {other:?}"),
    }
}
```

(`spawn_daemon_ai_slow` = `spawn_daemon_ai` with the delayed `FakeAi`.)

- [ ] **Step 2: Run to verify failure**

Run: `cargo nextest run -p norte-core --test daemon ai_rename`
Expected: FAIL — method unknown / dispatch falls through (`METHOD_NOT_FOUND` or similar), not the expected results.

- [ ] **Step 3: Add the dispatch arm**

In `crates/norte-core/src/daemon/server.rs`, inside `dispatch_fs_task`, next to the `INDEX_QUERY` arm (~line 2841):

```rust
        // ai.rename_plan (0.32.0, M4-IA, ADR 0031): plan de rename revisable.
        // Respuesta DIRECTA; cancelable (#72) — la llamada al proveedor tarda.
        methods::AI_RENAME_PLAN => {
            let p: methods::AiRenamePlanParams = parse_params(req.params)?;
            read_gate(&actor, &p.dir, shared)?; // #80
            let plan = shared
                .engine
                .ai_rename_plan(&p.dir, &p.instruction)
                .await
                .map_err(RpcError::from)?;
            let entries = plan
                .entries
                .into_iter()
                .map(|e| methods::AiRenameEntry {
                    // Invariante del engine: el plan solo contiene nombres
                    // UTF-8 (hostiles rechazados fail-loud pre-proveedor), la
                    // conversión lossy es identidad.
                    from: String::from_utf8_lossy(e.from.as_bytes()).into_owned(),
                    to: String::from_utf8_lossy(e.to.as_bytes()).into_owned(),
                })
                .collect();
            to_value(&methods::AiRenamePlanResult { entries })
        }
```

- [ ] **Step 4: Add to the cancelable set**

In `handle_value` (~line 1565):

```rust
            let cancelable = matches!(
                req.method.as_str(),
                methods::FS_COPY
                    | methods::FS_MOVE
                    | methods::FS_DELETE
                    | methods::FS_MKDIR
                    | methods::AI_RENAME_PLAN
            );
```

- [ ] **Step 5: Run the tests**

Run: `cargo nextest run -p norte-core --test daemon ai_rename`
Expected: PASS (3 tests).

- [ ] **Step 6: Wire AI into `norte daemon run`**

In `crates/norte-cli/src/main.rs`, `daemon_cmd` Run arm, between `engine.set_connector(...)` (~line 1190) and `Daemon::bind_with_policy` (~line 1191). Degrading like the index block at lines 1178-1185 — the daemon must NEVER abort startup over `[ai]`:

```rust
            // IA (M4-IA, ADR 0031): opt-in. Sin [ai], sin proveedor o con
            // config rota el engine degrada (ai.* → Unsupported / gate
            // PolicyDenied); jamás aborta el arranque del daemon.
            match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
                Ok(Ok(config)) => {
                    if let Some(pcfg) = config.rename_provider_config().cloned() {
                        match norte_core::ai::resolve_and_build(
                            &pcfg,
                            norte_core::connect::config_dir(),
                        )
                        .await
                        {
                            Ok(provider) => engine.set_ai_provider(provider),
                            Err(e) => eprintln!(
                                "aviso: proveedor de IA no disponible ({e}); ai.* dará Unsupported"
                            ),
                        }
                    }
                    engine.set_ai_config(config);
                }
                Ok(Err(e)) => eprintln!("aviso: [ai] inválido ({e}); ai.* dará Unsupported"),
                Err(e) => eprintln!("aviso: carga de [ai] falló ({e}); ai.* dará Unsupported"),
            }
```

- [ ] **Step 7: Crate checks**

Run: `cargo nextest run -p norte-core && cargo clippy -p norte-core --all-targets -- -D warnings && cargo clippy -p norte-cli --all-targets -- -D warnings`
Expected: PASS. (The daemon.rs `"0.1.0 no es N ni N-1 de 0.31.0"` expect-message at line 236 — update the string to 0.32.0 while there.)

- [ ] **Step 8: Commit**

```bash
git add crates/norte-core crates/norte-cli
git commit -m "feat(core,cli): ai.rename_plan daemon handler + startup AI wiring (M4-IA)"
```

---

### Task 3: `Backend::ai_rename_plan` + TUI embedded AI wiring

**Files:**
- Modify: `crates/norte-core/src/backend.rs` (public method near `index_query` at line 469; remote impl near line 2039; `call_timed_guarded` at line 1807)
- Modify: `crates/norte-tui/src/main.rs` (`make_backend` embedded arm, ~line 643-675)
- Test: `crates/norte-core/tests/` (embedded arm; remote is covered by Task 2's daemon tests)

- [ ] **Step 1: Generalize the guarded call's timeout**

`CALL_TIMEOUT` is 30s (backend.rs:1316) — too short for a model call. In `mod remote`, refactor `call_timed_guarded` (line 1807) into a timeout-parameterized core plus the existing wrapper:

```rust
        /// Timeout de llamadas de IA: el proveedor (modelo remoto) tarda
        /// legítimamente mucho más que un fs.*. Cancel-on-drop igualmente.
        const AI_CALL_TIMEOUT: Duration = Duration::from_secs(120);

        async fn call_timed_guarded_with<P, R>(
            &self,
            timeout: Duration,
            method: &str,
            params: &P,
        ) -> Result<R, Error>
        where
            P: serde::Serialize,
            R: serde::de::DeserializeOwned,
        {
            // (cuerpo EXACTO del call_timed_guarded actual, con `timeout`
            // en lugar de CALL_TIMEOUT en el tokio::time::timeout)
        }

        async fn call_timed_guarded<P, R>(&self, method: &str, params: &P) -> Result<R, Error>
        where
            P: serde::Serialize,
            R: serde::de::DeserializeOwned,
        {
            self.call_timed_guarded_with(CALL_TIMEOUT, method, params).await
        }
```

Move the body verbatim; only the timeout source changes. Update the rustdoc note "Solo lo usan las MUTACIONES" to add `ai.rename_plan` (it IS in the daemon's cancel arm since Task 2).

- [ ] **Step 2: Add the remote method**

Next to `index_query` in `mod remote` (~line 2039):

```rust
        pub(super) async fn ai_rename_plan(
            &self,
            dir: &VPath,
            instruction: &str,
        ) -> Result<methods::AiRenamePlanResult, Error> {
            self.call_timed_guarded_with(
                AI_CALL_TIMEOUT,
                methods::AI_RENAME_PLAN,
                &methods::AiRenamePlanParams {
                    dir: dir.clone(),
                    instruction: instruction.to_string(),
                },
            )
            .await
        }
```

- [ ] **Step 3: Add the public Backend method**

Near `index_query` (line 469), with the core→proto mapper:

```rust
    /// Plan de rename revisable de `dir` vía IA (M4-IA, ADR 0031). NO muta:
    /// aplicar el plan son N [`Backend::move_`] gobernados.
    ///
    /// # Errors
    /// [`Error::Unsupported`] sin proveedor de IA; [`Error::PolicyDenied`]
    /// del gate de IA (off, local-only, denied prefix); taxonomía del
    /// protocolo para fallos del proveedor.
    pub async fn ai_rename_plan(
        &self,
        dir: &VPath,
        instruction: &str,
    ) -> Result<norte_proto::methods::AiRenamePlanResult, Error> {
        match self {
            Self::Embedded(engine) => {
                let plan = engine.ai_rename_plan(dir, instruction).await?;
                Ok(ai_plan_to_proto(plan))
            }
            #[cfg(unix)]
            Self::Remote(r) => r.ai_rename_plan(dir, instruction).await,
        }
    }
```

Free function next to `index_hit_to_proto`:

```rust
/// Core → proto: el plan solo contiene nombres UTF-8 (invariante del engine:
/// hostiles rechazados fail-loud pre-proveedor) — lossy es identidad.
fn ai_plan_to_proto(plan: crate::ai::RenamePlan) -> norte_proto::methods::AiRenamePlanResult {
    norte_proto::methods::AiRenamePlanResult {
        entries: plan
            .entries
            .into_iter()
            .map(|e| norte_proto::methods::AiRenameEntry {
                from: String::from_utf8_lossy(e.from.as_bytes()).into_owned(),
                to: String::from_utf8_lossy(e.to.as_bytes()).into_owned(),
            })
            .collect(),
    }
}
```

(Task 2's daemon handler duplicates this mapping inline — switch the handler to call `crate::backend::ai_plan_to_proto` if visibility allows (`pub(crate)`), else leave both; prefer the shared fn.)

- [ ] **Step 4: Embedded-arm test**

In `crates/norte-core/tests/ai_rename.rs`, add (reusing the file's own `FakeAi` + `engine_with`):

```rust
#[tokio::test]
async fn backend_embebido_devuelve_el_plan_en_tipos_proto() {
    let engine = engine_with(r#"[{"from":"a.txt","to":"b.txt"}]"#, true, enabled());
    // (sembrar mem:///a.txt igual que los tests vecinos del fichero)
    let backend = norte_core::Backend::Embedded(std::sync::Arc::new(engine));
    let plan = backend
        .ai_rename_plan(&vp("mem:///"), "renombra")
        .await
        .expect("plan");
    assert_eq!(plan.entries.len(), 1);
    assert_eq!(plan.entries[0].to, "b.txt");
}
```

(Match the seeding/`vp` helpers already in that file; if `engine_with` returns a non-Arc `Engine`, wrap as the file's other tests do.)

- [ ] **Step 5: Wire AI into the TUI embedded engine**

In `crates/norte-tui/src/main.rs`, `make_backend`, embedded branch, after `engine.set_connector(...)` and before `return Ok(Backend::Embedded(...))` — same degrading shape as the daemon (Task 2 Step 6), with `app` not yet running just log to stderr:

```rust
        // IA (M4-IA): opt-in; sin [ai] el backend degrada (Unsupported).
        match tokio::task::spawn_blocking(norte_core::ai::AiConfig::load).await {
            Ok(Ok(ai_cfg)) => {
                if let Some(pcfg) = ai_cfg.rename_provider_config().cloned() {
                    match norte_core::ai::resolve_and_build(
                        &pcfg,
                        norte_core::connect::config_dir(),
                    )
                    .await
                    {
                        Ok(provider) => engine.set_ai_provider(provider),
                        Err(e) => eprintln!("aviso: proveedor de IA no disponible ({e})"),
                    }
                }
                engine.set_ai_config(ai_cfg);
            }
            Ok(Err(e)) => eprintln!("aviso: [ai] inválido ({e})"),
            Err(e) => eprintln!("aviso: carga de [ai] falló ({e})"),
        }
```

- [ ] **Step 6: Run and commit**

Run: `cargo nextest run -p norte-core --test ai_rename && cargo clippy -p norte-core --all-targets -- -D warnings && cargo clippy -p norte-tui --all-targets -- -D warnings`
Expected: PASS.

```bash
git add crates/norte-core crates/norte-tui
git commit -m "feat(core,tui): Backend::ai_rename_plan + embedded AI wiring (M4-IA)"
```

---

### Task 4: TUI — modals, state helpers, i18n, state tests

**Files:**
- Modify: `crates/norte-tui/src/app.rs` (Modal enum ~line 2167-2310; text-modal helpers ~line 1887-1970; allowlists ~line 2345)
- Modify: `crates/norte-tui/src/keymap.rs` (`commands!` ~line 110-145)
- Modify: `crates/norte-i18n/i18n/en.ftl`, `crates/norte-i18n/i18n/es.ftl`
- Test: `crates/norte-tui/tests/modal.rs`

- [ ] **Step 1: Add the command**

In `keymap.rs` `commands!` block: `"pane.ai-rename" => PaneAiRename,` (alphabetical among `pane.*`).

- [ ] **Step 2: Add the Modal variants**

In `app.rs`, after `Mkdir`:

```rust
    /// Prompt de instrucción del rename IA (M4-IA). Texto libre, molde Mkdir.
    AiRenameInstruction {
        /// Lo tecleado hasta ahora.
        instruction: String,
        /// Diagnóstico del último intento fallido, bajo el campo.
        error: Option<String>,
    },
    /// Plan de rename IA revisable (M4-IA): superficie de DECISIÓN. Enter/`y`
    /// aplica (contenido revisado por el humano); Esc/`n` descarta.
    AiRenamePlan {
        /// Dir sobre el que se aplican los moves.
        dir: VPath,
        /// Parejas from→to del modelo (proto, UTF-8 garantizado).
        entries: Vec<norte_proto::methods::AiRenameEntry>,
    },
```

- [ ] **Step 3: Add the state helpers (mirror the Mkdir set at app.rs:1887-1970 exactly)**

```rust
    /// Abre el prompt de instrucción del rename IA (M4-IA).
    pub fn open_ai_rename(&mut self) {
        if self.modal.is_none() {
            self.modal = Some(Modal::AiRenameInstruction { instruction: String::new(), error: None });
        }
    }

    /// Añade un char a la instrucción (clamp [`MARK_PATTERN_MAX_CHARS`]).
    pub fn ai_rename_push(&mut self, c: char) {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            if instruction.chars().count() < MARK_PATTERN_MAX_CHARS {
                instruction.push(c);
            }
            *error = None;
        }
    }

    /// Borra el último char.
    pub fn ai_rename_pop(&mut self) {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            instruction.pop();
            *error = None;
        }
    }

    /// Cierra el prompt sin lanzar nada.
    pub fn cancel_ai_rename(&mut self) {
        if matches!(self.modal, Some(Modal::AiRenameInstruction { .. })) {
            self.modal = None;
        }
    }

    /// Valida y devuelve la instrucción; NO cierra el modal (molde #104
    /// MINOR-1: el caller cierra con [`Self::ai_rename_submitted`] SOLO tras
    /// lanzar la petición; un fallo deja el texto y el diagnóstico).
    pub fn ai_rename_confirm(&mut self) -> Option<String> {
        if let Some(Modal::AiRenameInstruction { instruction, error }) = &mut self.modal {
            let text = instruction.trim();
            if text.is_empty() {
                *error = Some(t("modal-ai-rename-empty-instruction"));
                return None;
            }
            return Some(text.to_owned());
        }
        None
    }

    /// Cierra el prompt tras lanzar la petición.
    pub fn ai_rename_submitted(&mut self) {
        if matches!(self.modal, Some(Modal::AiRenameInstruction { .. })) {
            self.modal = None;
        }
    }

    /// Deja el diagnóstico de un lanzamiento fallido; el texto sobrevive.
    pub fn ai_rename_set_error(&mut self, msg: String) {
        if let Some(Modal::AiRenameInstruction { error, .. }) = &mut self.modal {
            *error = Some(msg);
        }
    }
```

(Match the surrounding style; the `cancel_mkdir` `debug_assert!` guard at app.rs:1918 — extend its free-text whitelist with `AiRenameInstruction` if the guard enumerates variants.)

- [ ] **Step 4: Allowlist for the plan modal**

The plan modal is a decision surface over **user-initiated** content already reviewed on screen — `ALLOW_CONFIRM` semantics (same as `ConfirmDelete`), NOT `ALLOW_APPROVAL` (that doctrine is for agent-initiated ops). In `dialog_action` (app.rs ~2345-2500), route `Modal::AiRenamePlan` with `ALLOW_CONFIRM` → `DialogOutcome::Confirmed | Cancelled`.

- [ ] **Step 5: i18n keys (BOTH en.ftl and es.ftl)**

en.ftl:

```ftl
modal-ai-rename = AI rename — instruction
modal-ai-rename-hint = Enter: request plan · Esc: cancel
modal-ai-rename-empty-instruction = type an instruction first
modal-ai-rename-plan = AI rename — proposed plan
modal-ai-rename-pair-from = { $badge }{ $from }
modal-ai-rename-pair-to = → { $to }
modal-ai-rename-more = … and { $n } more
modal-ai-rename-plan-hint = y: apply · n/Esc: discard
msg-ai-rename-running = AI rename: thinking… (Esc cancels)
msg-ai-rename-empty = AI rename: the model proposed no changes
msg-ai-rename-failed = AI rename failed: { $error }
msg-ai-rename-applied = AI rename: { $n } move(s) submitted
msg-ai-rename-in-search = AI rename is not available in a search pane
help-cmd-pane-ai-rename = AI rename of the current directory (reviewable plan)
```

es.ftl (translate; same ids). Parity is enforced by `message_ids` tests.

- [ ] **Step 6: Modal state tests**

In `crates/norte-tui/tests/modal.rs`, pattern-match the file's existing style:

```rust
#[test]
fn ai_rename_instruccion_conserva_texto_tras_fallo() {
    let mut app = app(); // el helper del fichero
    app.open_ai_rename();
    for c in "kebab".chars() { app.ai_rename_push(c); }
    assert_eq!(app.ai_rename_confirm().as_deref(), Some("kebab"));
    app.ai_rename_set_error("boom".into());
    // El modal sigue abierto con el texto y el error.
    match &app.modal {
        Some(norte_tui::app::Modal::AiRenameInstruction { instruction, error }) => {
            assert_eq!(instruction, "kebab");
            assert_eq!(error.as_deref(), Some("boom"));
        }
        other => panic!("modal inesperado: {other:?}"),
    }
    app.ai_rename_submitted();
    assert!(app.modal.is_none());
}

#[test]
fn ai_rename_confirm_vacio_no_devuelve_y_deja_diagnostico() {
    let mut app = app();
    app.open_ai_rename();
    assert!(app.ai_rename_confirm().is_none());
    assert!(matches!(
        &app.modal,
        Some(norte_tui::app::Modal::AiRenameInstruction { error: Some(_), .. })
    ));
}
```

- [ ] **Step 7: Run and commit**

Run: `cargo nextest run -p norte-tui --test modal && cargo nextest run -p norte-i18n && cargo clippy -p norte-tui --all-targets -- -D warnings`
Expected: PASS (new state tests + i18n parity + `todo_comando_tiene_ayuda_traducida`; the `dispatch` match will NOT compile until Task 5 adds the arm — if so, add a placeholder-free arm now: `Command::PaneAiRename => app.open_ai_rename(),` refined in Task 5).

```bash
git add crates/norte-tui crates/norte-i18n
git commit -m "feat(tui): AI rename modals, state helpers, i18n (M4-IA)"
```

---

### Task 5: TUI — run-loop wiring, in-flight run, render, render tests

**Files:**
- Modify: `crates/norte-tui/src/main.rs` (Tier-A intercept block ~line 1294-1390; dispatch ~line 3908+; a new `AiRenameRun` next to `SearchRun` at ~line 75)
- Modify: `crates/norte-tui/src/ui.rs` (`modal_height` ~line 1050, `modal_title_body` ~line 1112, new text fns near `approval_modal_text` ~line 1284)
- Test: `crates/norte-tui/tests/render.rs`

- [ ] **Step 1: In-flight run state**

Next to `SearchRun` (main.rs ~75):

```rust
/// Petición ai.rename_plan EN VUELO (M4-IA). Dropearla cancela (regla 3):
/// abort → el future del backend se dropea → CancelOnAbandon envía
/// `rpc.cancel` (remoto) / aborta el ChatStream (embebido).
struct AiRenameRun {
    handle: tokio::task::JoinHandle<Result<norte_proto::methods::AiRenamePlanResult, norte_proto::Error>>,
    /// Dir del pane al lanzar; el plan se aplica AQUÍ aunque el usuario navegue.
    dir: VPath,
}
```

Declared in the run loop as `let mut ai_rename_run: Option<AiRenameRun> = None;` (same place `search_run` lives).

- [ ] **Step 2: Dispatch arm**

In `dispatch` (guard like `PaneMkdir` at main.rs:4060):

```rust
            Command::PaneAiRename => {
                if app.focused().virtual_search {
                    app.message = Some(t("msg-ai-rename-in-search"));
                } else {
                    app.open_ai_rename();
                }
            }
```

- [ ] **Step 3: Tier-A intercept for the instruction modal**

In the modal key block (after the `Mkdir` intercept at main.rs:1352, same shape):

```rust
                        if matches!(app.modal, Some(Modal::AiRenameInstruction { .. })) {
                            let plain = key.modifiers.is_empty()
                                || key.modifiers == KeyModifiers::SHIFT;
                            match key.code {
                                KeyCode::Char(c) if plain => app.ai_rename_push(c),
                                KeyCode::Backspace if plain => app.ai_rename_pop(),
                                KeyCode::Enter if plain => {
                                    if let Some(instruction) = app.ai_rename_confirm() {
                                        let dir = app.focused().dir.clone();
                                        let b = backend.clone();
                                        let d = dir.clone();
                                        let handle = tokio::spawn(async move {
                                            b.ai_rename_plan(&d, &instruction).await
                                        });
                                        ai_rename_run = Some(AiRenameRun { handle, dir });
                                        app.message = Some(t("msg-ai-rename-running"));
                                        app.ai_rename_submitted();
                                    }
                                }
                                KeyCode::Esc if plain => app.cancel_ai_rename(),
                                _ => {}
                            }
                            continue;
                        }
```

(`app.focused().dir` — use whatever accessor the `PaneMkdir`/`launch_search` paths use for the active pane's directory; `launch_search` at main.rs:3612 shows it.)

- [ ] **Step 4: Poll the run each loop iteration + Esc cancels**

Where `drain_search(...)` is called each iteration, add:

```rust
        // ai.rename_plan en vuelo (M4-IA): cosecha sin bloquear.
        if ai_rename_run.as_ref().is_some_and(|r| r.handle.is_finished()) {
            let run = ai_rename_run.take().expect("comprobado is_some");
            match run.handle.await {
                Ok(Ok(plan)) if plan.entries.is_empty() => {
                    app.message = Some(t("msg-ai-rename-empty"));
                }
                Ok(Ok(plan)) => {
                    app.message = None;
                    app.modal = Some(Modal::AiRenamePlan { dir: run.dir, entries: plan.entries });
                }
                Ok(Err(e)) => {
                    app.message = Some(ta(
                        "msg-ai-rename-failed",
                        &[("error", &detail_for_bar(&error_category(&e)))],
                    ));
                }
                Err(_join) => {} // abortado por Esc: silencio, ya se avisó
            }
        }
```

Esc handling: in the branch where Esc reaches the pane (no modal — same site as the search-Esc dispatch at main.rs:1451), before/alongside it:

```rust
                        // Esc con ai.rename_plan en vuelo = cancelar (regla 3).
                        if let Some(run) = ai_rename_run.take() {
                            run.handle.abort();
                            app.message = None;
                            continue;
                        }
```

- [ ] **Step 5: Plan-modal decision handling**

In `on_dialog_key`'s modal match (main.rs:3252+), arm for `Modal::AiRenamePlan` on `DialogOutcome::Confirmed` → apply; `Cancelled` → close. Apply mirrors the CLI loop (crates/norte-cli/src/main.rs:1382-1400) but submits tasks to the board without joining (the board tracks them):

```rust
async fn apply_ai_rename(
    app: &mut App,
    backend: &Backend,
    dir: &VPath,
    entries: &[norte_proto::methods::AiRenameEntry],
) {
    let mut n = 0usize;
    for e in entries {
        // Segment::new valida (el engine ya validó `to`; cinturón igualmente).
        let (Ok(from), Ok(to)) = (
            norte_proto::Segment::new(e.from.clone().into_bytes()),
            norte_proto::Segment::new(e.to.clone().into_bytes()),
        ) else {
            continue;
        };
        match backend
            .move_(&dir.join(from), &dir.join(to), TransferOptions::default())
            .await
        {
            Ok(task) => {
                app.board.push(task, None);
                n += 1;
            }
            Err(e) => {
                app.message = Some(ta(
                    "msg-ai-rename-failed",
                    &[("error", &detail_for_bar(&error_category(&e)))],
                ));
                break; // primer fallo para: lo encolado ya está en el board
            }
        }
    }
    if n > 0 {
        app.message = Some(ta("msg-ai-rename-applied", &[("n", &n.to_string())]));
    }
}
```

(`Segment::new`'s exact signature: match the call form `mkdir_confirm` uses at app.rs:1939. Apply order is the plan's order — the engine's `validate_rename_reply` only admits plans whose collisions are renamed away, and the CLI applies in plan order; keep that behaviour.)

- [ ] **Step 6: Render**

In `ui.rs` — `modal_height` arm for both variants; `modal_title_body` dispatch; two text fns modelled on `mkdir_modal_text` (line 1351) and `approval_modal_text` (line 1284):

```rust
/// Prompt de instrucción (M4-IA): campo + hint, molde mkdir.
fn ai_rename_modal_text(instruction: &str, error: Option<&str>) -> (String, String) {
    let mut lines = vec![format!("> {instruction}"), t("modal-ai-rename-hint")];
    if let Some(e) = error {
        lines.push(clamp_chars(e, 60));
    }
    (t("modal-ai-rename"), lines.join("\n"))
}

/// Tope de parejas pintadas; el resto se resume (molde MODAL_ITEM_LIMIT).
const AI_RENAME_PAIR_LIMIT: usize = 5;

/// Plan revisable (M4-IA, doctrina encoding-auditor): cada nombre en SU línea,
/// `→` fuera de banda al inicio de la línea del destino (jamás joiner in-band
/// que un nombre pueda imitar), elipsis media, enmascarado MARCADO con badge.
fn ai_rename_plan_modal_text(
    entries: &[norte_proto::methods::AiRenameEntry],
) -> (String, String) {
    let mut lines = Vec::new();
    for e in entries.iter().take(AI_RENAME_PAIR_LIMIT) {
        let (from, from_hostil) = display_name(e.from.as_bytes());
        let (to, to_hostil) = display_name(e.to.as_bytes());
        lines.push(ta(
            "modal-ai-rename-pair-from",
            &[
                ("badge", if from_hostil { HOSTILE_BADGE } else { "" }),
                ("from", &middle_ellipsis(&from, 46)),
            ],
        ));
        lines.push(ta(
            "modal-ai-rename-pair-to",
            &[("to", &format!(
                "{}{}",
                if to_hostil { HOSTILE_BADGE } else { "" },
                middle_ellipsis(&to, 44),
            ))],
        ));
    }
    if entries.len() > AI_RENAME_PAIR_LIMIT {
        let n = entries.len() - AI_RENAME_PAIR_LIMIT;
        lines.push(ta("modal-ai-rename-more", &[("n", &n.to_string())]));
    }
    lines.push(t("modal-ai-rename-plan-hint"));
    (t("modal-ai-rename-plan"), lines.join("\n"))
}
```

(`display_name` returns `(String, bool)`; even though entries are engine-guaranteed UTF-8, a MITM'd/N+1 daemon could send anything — render defensively, always. Adjust the fluent arg plumbing to match how `approval_modal_text` builds `ta` args.)

- [ ] **Step 7: Render test (hostile plan)**

In `crates/norte-tui/tests/render.rs`, pattern of `modal_de_aprobacion_...` (line 361):

```rust
/// M4-IA (doctrina encoding-auditor): el plan pinta contenido del MODELO —
/// controles/bidi → `�` con badge; un `from` kilométrico no expulsa el `to`;
/// `→` fuera de banda en su propia línea.
#[test]
fn modal_de_plan_ai_enmascara_y_no_oculta_el_destino() {
    let dir = vp("file:///x");
    let mut app = App::new(Pane::new(dir.clone(), Vec::new()), Pane::new(dir.clone(), Vec::new()));
    let from_largo = format!("{}\u{202e}oculto.txt", "x".repeat(120));
    app.modal = Some(norte_tui::app::Modal::AiRenamePlan {
        dir,
        entries: vec![norte_proto::methods::AiRenameEntry {
            from: from_largo,
            to: "destino-final.txt".into(),
        }],
    });
    let mut terminal = Terminal::new(TestBackend::new(60, 14)).expect("terminal");
    terminal.draw(|f| ui::draw(f, &app)).expect("draw");
    let contenido = terminal.backend().to_string();
    assert!(contenido.contains("destino-final"), "el destino jamás se expulsa: {contenido}");
    assert!(contenido.contains('\u{FFFD}'), "bidi → �: {contenido}");
    assert!(contenido.contains('!'), "enmascarado MARCADO: {contenido}");
    assert!(contenido.contains("→"), "flecha fuera de banda: {contenido}");
}
```

- [ ] **Step 8: Run everything and commit**

Run: `cargo nextest run -p norte-tui && cargo clippy -p norte-tui --all-targets -- -D warnings`
Expected: PASS (snapshots via insta may need `cargo insta review` if any existing snapshot shifted — none should).

```bash
git add crates/norte-tui
git commit -m "feat(tui): AI rename end-to-end — run loop, plan modal, render (M4-IA)"
```

---

### Task 6: GUI — session plumbing, modals, command

**Files (all under `crates/norte-gui/`, outside the workspace — build with `cd crates/norte-gui && cargo …`):**
- Modify: `src/session.rs` (`SessionCmd`/`SessionEvent` + the match arm ~line 380-560)
- Modify: `src/modal.rs` (Modal + PendingOp + ModalOutcome + on_key)
- Modify: `src/keymap.rs` (COMMANDS ~line 16)
- Modify: `src/main.rs` (dispatch, `render_modal`/`modal_lines`, event handling)
- i18n: `crates/norte-i18n/i18n/{en,es}.ftl` (gui keys; workspace crate — commit together)

- [ ] **Step 1: Session command/event**

In `session.rs`:

```rust
    /// Pide el plan de rename IA de `dir` (M4-IA).
    AiRenamePlan { dir: VPath, instruction: String },
```

Event (payload proto entries — already-safe wire type; the view sanitizes at render):

```rust
    /// Plan de rename IA listo (M4-IA): entries, o diagnóstico ya saneado.
    AiRenamePlan { dir: VPath, result: Result<Vec<norte_proto::methods::AiRenameEntry>, String> },
```

Match arm (mirror `SessionCmd::List` at :383 — clone remote, `tokio::spawn`, send event):

```rust
            SessionCmd::AiRenamePlan { dir, instruction } => {
                let backend = Backend::Remote(remote.clone());
                let tx = event_tx.clone();
                tokio::spawn(async move {
                    let result = backend
                        .ai_rename_plan(&dir, &instruction)
                        .await
                        .map(|r| r.entries)
                        .map_err(|e| sanitized_error(&e)); // el helper de error que usen los arms vecinos
                    tx.send(SessionEvent::AiRenamePlan { dir, result }).ok();
                });
            }
```

(Use the same error-to-string sanitization the neighbouring arms use — grep how `Submit`/`List` report errors; do not invent a new helper if one exists.)

- [ ] **Step 2: Modal variants + key handling**

In `modal.rs` (pure state machine — testable without GPUI):

```rust
    /// Prompt de instrucción del rename IA (M4-IA). Query en BYTES (molde
    /// PaletteView): push_char/backspace UTF-8-boundary-aware.
    AiRenamePrompt { dir: VPath, query: Vec<u8> },
    /// Plan revisable (M4-IA): `y` aplica, `n`/`escape` descarta.
    AiRenamePlan { dir: VPath, entries: Vec<norte_proto::methods::AiRenameEntry> },
```

`ModalOutcome`: add `RequestAiPlan { dir: VPath, instruction: String }` and reuse `Submit(Vec<PendingOp>)` for the apply (each pair → `PendingOp::Transfer { kind: Move, from, to, opts }` — match the existing `PendingOp::Transfer` shape at modal.rs:23). `on_key` arms:

- `AiRenamePrompt`: printable char → push bytes (copy `PaletteView::push_char`/`backspace` byte handling, palette_view.rs:116-136); `"enter"` → `RequestAiPlan` (empty query → `StayOpen`); `"escape"` → `Dismiss`.
- `AiRenamePlan`: `"y"` → `Submit(moves)`; `"n"` | `"escape"` → `Dismiss`; else `Ignored`.

- [ ] **Step 3: Command + dispatch + render**

- `keymap.rs` COMMANDS: add `"pane.ai-rename"` (help id `help-cmd-pane-ai-rename` already added in Task 4 — shared Fluent family).
- `main.rs`: dispatch arm opens `Modal::AiRenamePrompt { dir: <active pane dir>, query: Vec::new() }`; on `ModalOutcome::RequestAiPlan` send `SessionCmd::AiRenamePlan` and show a "thinking" status line; on `SessionEvent::AiRenamePlan` open `Modal::AiRenamePlan` (or status message for empty/error).
- `modal_lines` arm (main.rs:4191): prompt renders the lossy+hazard-masked query (copy `query_display`, palette_view.rs:169); plan renders pairs as TWO lines each (`from` line, `→ to` line) through the GUI's existing hazard-masking path (the same one `modal_lines_nunca_deja_hazards_crudos_del_corpus_hostil` at main.rs:5811 exercises), capped at 5 pairs + "… and N more" line.
- New Fluent keys `gui-modal-ai-rename-*` in both locales if the GUI needs distinct strings; reuse `modal-ai-rename-*` where identical.

- [ ] **Step 4: Tests**

In the GUI's own test style (modal state machine + hostile lines):

```rust
#[test]
fn ai_prompt_enter_pide_plan_y_esc_descarta() {
    let mut m = Modal::AiRenamePrompt { dir: vp("file:///d"), query: b"kebab".to_vec() };
    match modal::on_key(&mut m, "enter") {
        ModalOutcome::RequestAiPlan { instruction, .. } => assert_eq!(instruction, "kebab"),
        other => panic!("{other:?}"),
    }
    let mut m = Modal::AiRenamePrompt { dir: vp("file:///d"), query: Vec::new() };
    assert!(matches!(modal::on_key(&mut m, "enter"), ModalOutcome::StayOpen));
    assert!(matches!(modal::on_key(&mut m, "escape"), ModalOutcome::Dismiss));
}

#[test]
fn ai_plan_y_aplica_como_moves_y_n_descarta() {
    let entries = vec![norte_proto::methods::AiRenameEntry { from: "a.txt".into(), to: "b.txt".into() }];
    let mut m = Modal::AiRenamePlan { dir: vp("file:///d"), entries: entries.clone() };
    match modal::on_key(&mut m, "y") {
        ModalOutcome::Submit(ops) => assert_eq!(ops.len(), 1),
        other => panic!("{other:?}"),
    }
    let mut m = Modal::AiRenamePlan { dir: vp("file:///d"), entries };
    assert!(matches!(modal::on_key(&mut m, "n"), ModalOutcome::Dismiss));
}
```

Extend the hostile-lines sweep (`modal_lines_nunca_deja_hazards_crudos_del_corpus_hostil`, main.rs:5811) with an `AiRenamePlan` case fed from `norte_testkit::corpus::hostile_names()`.

- [ ] **Step 5: Run and commit**

Run: `cd crates/norte-gui && cargo nextest run && cargo clippy --all-targets -- -D warnings && cargo fmt --check`
Expected: PASS. Also `just check-gui` from the repo root.

```bash
git add crates/norte-gui crates/norte-i18n
git commit -m "feat(gui): AI rename — prompt, reviewable plan, session plumbing (M4-IA)"
```

---

### Task 7: Close-out — reviewers, changelog, deferred issue, full CI

**Files:**
- Modify: `CHANGELOG.md`
- No code except review fixes.

- [ ] **Step 1: CHANGELOG entry** (under `[Unreleased]` `### Added`):

```markdown
- **AI rename over the wire (M4-IA, ADR 0031, proto 0.32.0):** new
  `ai.rename_plan` method (direct, cancellable via `rpc.cancel`); the daemon
  builds the configured AI provider at startup (degrading, opt-in). TUI and
  GUI gain a reviewable-plan AI rename (`pane.ai-rename`): instruction prompt
  → plan review → N journaled `fs.move` with undo.
```

- [ ] **Step 2: Deferred-scope issue**

```bash
gh issue create --title "AI rename over the first-class selection (#103)" \
  --body "M4-IA v1 renames the whole directory (Engine::ai_rename_plan signature). Extend to operate on the active selection (#103 machinery): engine gains a names subset param, TUI/GUI pass the selection when non-empty. Spec: docs/superpowers/specs/2026-08-03-m4-ia-design.md (Deferred)."
```

- [ ] **Step 3: Reviewers** (project practice — dispatch and APPLY findings):

- `protocol-guardian` on the Task 1 diff (proto 0.32.0) — mandatory for proto.
- `rust-reviewer` on the full IA-1 diff.
- `encoding-auditor` on TUI/GUI rendering (Tasks 4-6).
- `security-reviewer` on the daemon startup wiring + wire exposure (Task 2).

- [ ] **Step 4: Full gate**

Run: `just ci`
Expected: EXIT=0 (includes `check-gui`; coverage gate 85% on proto/vfs/core — the new daemon arm and backend method are covered by Task 2/3 tests. If llvm-cov shows stale numbers, `cargo llvm-cov clean` first).

- [ ] **Step 5: Commit review fixes**

```bash
git add -A
git commit -m "fix(tui,core): M4-IA review findings"
```
