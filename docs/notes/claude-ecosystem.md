# Ecosistema Claude Code local para `norte`

Estructura bajo el repo (`.claude/`) + un `justfile` como interfaz única de comandos. Filosofía: Claude Code como par de ingeniería con especialistas (subagentes) para las zonas de riesgo del dominio (encodings, protocolo, seguridad), hooks que hacen imposible commitear basura, y slash commands para los rituales repetitivos.

```
norte/
├── CLAUDE.md                        # (ya generado)
├── justfile                         # ci, cov, fuzz, bench — Claude y humanos usan lo mismo
├── .claude/
│   ├── settings.json                # permisos + hooks
│   ├── agents/
│   │   ├── rust-reviewer.md
│   │   ├── encoding-auditor.md
│   │   ├── protocol-guardian.md
│   │   ├── test-engineer.md
│   │   └── security-reviewer.md
│   ├── commands/
│   │   ├── adr.md
│   │   ├── new-crate.md
│   │   ├── new-provider.md
│   │   ├── fixture.md
│   │   └── release-check.md
│   └── skills/
│       ├── vfs-provider/SKILL.md
│       ├── wasm-plugin/SKILL.md
│       └── proto-change/SKILL.md
└── docs/adr/
```

## 1. `.claude/settings.json`

```json
{
  "permissions": {
    "allow": [
      "Bash(cargo build*)", "Bash(cargo nextest*)", "Bash(cargo clippy*)",
      "Bash(cargo fmt*)", "Bash(cargo doc*)", "Bash(cargo llvm-cov*)",
      "Bash(cargo deny*)", "Bash(just *)", "Bash(git status*)", "Bash(git diff*)",
      "Bash(git log*)", "Bash(git add*)", "Bash(git commit*)"
    ],
    "deny": [
      "Bash(git push --force*)", "Bash(rm -rf /*)", "Read(.env*)",
      "Read(**/secrets/**)", "Bash(curl * | sh)", "Bash(cargo publish*)"
    ]
  },
  "hooks": {
    "PostToolUse": [
      {
        "matcher": "Edit|Write",
        "hooks": [{ "type": "command",
          "command": "f=$(jq -r '.tool_input.file_path // empty'); case \"$f\" in *.rs) cargo fmt -- \"$f\" 2>/dev/null || true;; esac" }]
      }
    ],
    "PreToolUse": [
      {
        "matcher": "Bash(git commit*)",
        "hooks": [{ "type": "command",
          "command": "cargo clippy --workspace --all-targets -q -- -D warnings && cargo nextest run --workspace -q" }]
      }
    ]
  }
}
```

(El hook `PreToolUse` en commit convierte el DoD en física: no se puede commitear con clippy o tests rojos ni "por esta vez".)

## 2. Subagentes (`.claude/agents/`)

**`rust-reviewer.md`** — Revisión de PR/diff antes de commit.
```markdown
---
name: rust-reviewer
description: Revisa diffs de Rust contra las reglas duras de CLAUDE.md. Usar proactivamente antes de cada commit sustancial.
tools: Read, Grep, Glob, Bash
---
Eres revisor senior de Rust en norte. Revisa el diff actual (git diff) contra:
las 10 reglas duras de CLAUDE.md; unwrap/expect fuera de tests; to_str() sobre
paths; std::fs fuera de norte-vfs-local; I/O en contexto async sin spawn_blocking;
Tasks sin chequeo de cancelación; deps nuevas sin justificación; API pública sin
rustdoc/doctest. Salida: lista priorizada BLOCKER/MAJOR/MINOR con ubicación
archivo:línea y fix propuesto. No arregles nada tú: solo informa.
```

**`encoding-auditor.md`** — El especialista del §6 del spec.
```markdown
---
name: encoding-auditor
description: Audita todo código que toque paths, nombres de archivo, texto o archivos comprimidos. Usar siempre que se modifique norte-vfs*, viewer o search.
tools: Read, Grep, Glob, Bash
---
Eres el auditor de encodings de norte. Caza: String donde debe haber VPath/OsString;
decodificación sin detección (asunciones UTF-8); comparaciones sin normalizar NFC;
concatenación de paths por strings; entradas ZIP decodificadas sin mirar el bit 11;
lecturas de texto sin pasar por el detector; pérdida silenciosa en conversiones
(lossy sin marcar). Para cada hallazgo: por qué corrompe datos, en qué OS, y qué
fixture de norte-testkit lo cubriría. Si la fixture no existe, propón su contenido.
```

**`protocol-guardian.md`** — Guardián de compatibilidad.
```markdown
---
name: protocol-guardian
description: Debe usarse ante cualquier cambio en norte-proto o en handlers JSON-RPC del core.
tools: Read, Grep, Glob, Bash
---
Custodias el wire format. Ante un diff en norte-proto o handlers: clasifica cada
cambio (aditivo-compatible / breaking); verifica que los golden tests cambiaron en
consecuencia; exige bump de versión de protocolo si hay breaking; comprueba que
campos nuevos son Option con default serde; verifica regeneración del JSON Schema.
Recuerda: N y N-1 deben coexistir. Si detectas breaking sin ADR, marca BLOCKER.
```

**`test-engineer.md`** — Escribe los tests que nadie quiere escribir.
```markdown
---
name: test-engineer
description: Diseña y escribe tests (unit, proptest, integration, cancelación) para código nuevo. Usar tras implementar cualquier feature.
tools: Read, Grep, Glob, Bash, Edit, Write
---
Eres ingeniero de test de norte. Para el código indicado: identifica la matriz de
casos (feliz, borde por OS, hostil del corpus testkit, cancelación, fallo inyectado
con MemProvider); escribe primero la lista, pide confirmación, luego implementa con
cargo nextest como runner. Property-based con proptest para todo lo que parsee o
normalice. Toda operación mutante: test de undo vía journal. Toda Task: test de
cancelación limpia. Cobertura objetivo del crate: 85%; compruébalo con llvm-cov.
```

**`security-reviewer.md`**
```markdown
---
name: security-reviewer
description: Revisión de seguridad para plugin-host, policy engine, mcp, daemon (auth de socket) y manejo de secretos.
tools: Read, Grep, Glob, Bash
---
Revisas contra el threat model (SECURITY.md): escape del sandbox WASM (capability
no comprobada), path traversal desde nombres hostiles (../../ en zip/sftp), zip
bombs sin límite, secretos en logs/config, sockets sin peer-cred, operaciones de
agente que puentean el policy engine, TOCTOU en checks de scope. Salida con
severidad y explotación plausible. Sé paranoico: este código gobernará agentes.
```

## 3. Slash commands (`.claude/commands/`)

- **`/adr <título>`** → crea `docs/adr/NNNN-slug.md` (MADR: contexto, opciones consideradas, decisión, consecuencias), numera secuencialmente, enlaza desde el índice, y deja el commit preparado.
- **`/new-crate <nombre>`** → scaffolding de crate del workspace: `Cargo.toml` con lints heredados del workspace, licencia correcta según tabla del spec (§16.2), `lib.rs` con `#![forbid(unsafe_code)]` y `#![warn(missing_docs)]`, módulo de tests, entrada en `ARCHITECTURE.md`.
- **`/new-provider <scheme>`** → scaffolding de provider VFS: impl del trait con `todo!()` documentados, declaración de `Capabilities`, suite de tests contractual (compartida vía `norte-testkit::provider_contract!`) ya enganchada, checklist de semántica (symlinks, case, trash) a rellenar.
- **`/fixture <descripción>`** → añade una fixture hostil al corpus de testkit (genera bytes exactos, documenta el caso real que representa, referencia el issue/OS).
- **`/release-check`** → dry-run de release: semver-checks, deny, changelog de release-plz, docs build, schema del protocolo regenerado y diffeado.

## 4. Skills (`.claude/skills/`)

Skills = conocimiento profundo bajo demanda (se cargan al activarse, no queman contexto):

- **`vfs-provider`** — Guía completa para escribir un provider: semántica exacta de cada método del trait, tabla de capabilities y sus implicaciones en el copy engine, los 12 errores clásicos (con ejemplos de sftp/s3), cómo pasar la suite contractual. Incluye `reference.md` con el mapeo errno→taxonomía por OS.
- **`wasm-plugin`** — El mundo WIT `norte:plugin` completo, cómo compilar desde Rust/Go, el modelo de permisos y cómo testear enforcement, plantilla de previewer y de column.
- **`proto-change`** — Procedimiento de cambio de protocolo: árbol de decisión compatible/breaking, cómo escribir el golden test, regeneración de schema, política N/N-1, ejemplos históricos.

## 5. MCP servers recomendados para la sesión de desarrollo

En `.mcp.json` del repo (compartido por el equipo):

- **GitHub MCP** — issues/PRs/CI sin salir de la sesión (revisar el nightly de fuzzing, abrir issue desde un hallazgo del security-reviewer).
- **`cargo doc` local via docs server** (o simplemente `cargo doc --open` + Read): para APIs de wasmtime/GPUI que cambian rápido, mejor la doc local de la versión pineada que la memoria del modelo.
- Cuando exista el M3: **el propio norte-mcp en dev** conectado a la sesión — Claude Code gestiona el árbol de fixtures a través de norte, dogfooding del producto mientras se desarrolla. Este bucle (el agente usa lo que construye) es oro para detectar fricción de la API agéntica.

## 6. Flujo de trabajo tipo (sesión Claude Code)

1. `claude` en la raíz → lee CLAUDE.md automáticamente.
2. Plan mode para la feature (spec §N como referencia) → plan aprobado.
3. Implementación; los hooks formatean y bloquean commits sucios.
4. `test-engineer` para la matriz de tests; `encoding-auditor` si se tocó vfs/viewer.
5. `rust-reviewer` sobre el diff final; `/adr` si hubo decisión estructural.
6. Commit convencional → PR pequeña → CI matrix.
