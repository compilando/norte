# Propuesta de plan M2 — "remotos + archivos"

Criterio de salida (spec §15): **"copiar de sftp a zip local vía S3 sin
sorpresas"**. Alcance spec: daemon JSON-RPC (decisión de M1: aplazado a
aquí), sftp, object storage, archive read (zip/tar), copy engine
cross-provider con resume.

Método idéntico a M0/M1: una fase = un commit convencional con `just ci`
verde + push + CI de GitHub verde; test-first en paths/encoding/wire;
reviewers sobre el diff antes de commit (rust-reviewer siempre;
protocol-guardian en proto/handlers; encoding-auditor en vfs/nombres;
**security-reviewer en daemon/auth/secretos — estrena protagonismo en
M2**); E2E real por fase (pty para TUI, contenedores para remotos); deuda
→ issue con hito; ADR para toda decisión de protocolo/deps/seguridad.

## Fases propuestas

| # | Fase | Contenido | Notas |
|---|------|-----------|-------|
| 1 | Deuda dura del engine | #16 identidad real de nodo ((dev,ino)/FileId) para los guards de Overwrite; #17 retry de mutaciones (ambigüedad post-efecto); #18 SymlinkKind real (dir-symlinks Windows); #19 Follow sobre dir-symlinks con visited set | Endurecer ANTES de multiplicar providers — igual que la fase 1 de M1 |
| 2 | Protocolo: envelope + daemon | Envelope JSON-RPC 2.0 (Request/Response/Notification, ids) en `norte-proto::wire`; transporte UDS/named pipe; auth por peer credentials (SO_PEERCRED / SID del pipe, spec §17.6) — solo el mismo usuario; lifecycle: autoarranque por el primer cliente, shutdown por inactividad, `--graceful`; un daemon por usuario, jamás root | Cambio de wire → golden + bump + ADR (transporte/framing/sesiones) + protocol-guardian + security-reviewer |
| 3 | Frontends contra el daemon | TUI/CLI eligen embebido\|daemon (config + flag; embebido sigue siendo el default de arranque instantáneo); reconexión con aviso; **dos frontends sobre la misma sesión** (TUI + CLI viendo las mismas tasks) | La regla 7 ya garantiza que solo cambia el transporte |
| 4 | Resume de transferencias | `.norte-partial` + offset (el `read` con rango del trait está desde M1-f2); verificación opcional por hash; GC de parciales huérfanos; contrato: cancelar deja destino limpio o `.norte-partial`, JAMÁS un archivo a medias sin marcar (regla ya vigente, ahora con reanudación) | Contrato nuevo en `provider_contract!`; OJO: resume por offset exige APPEND/RANDOM_WRITE — en S3 la reanudación es multipart, NO offset (diseñarlo en el ADR para no casarse con POSIX) |
| 5 | `norte-vfs-sftp` | russh (spec §4); capabilities honestas; servidor hostil del threat model (nombres `../../`, symlinks trampa); latencia/desconexión del MemProvider reutilizadas como espejo de tests; testcontainers openssh (nightly) + suite contractual local | Corpus de nombres hostiles del testkit contra un provider remoto DE VERDAD por primera vez |
| 6 | Conexiones + secretos | `connections.toml` (SOLO referencias); keyring del OS (Secret Service/Keychain/Credential Manager); env vars para CI/corporativo; UX mínima TUI/CLI para abrir `sftp://…` | Regla dura 10; security-reviewer obligatorio; ADR |
| 7 | `norte-vfs-object` | opendal con S3 primero (backends feature-gated — GCS/Azure después sin tocar código); `copy_native` = CopyObject con el contrato anti-sobrescritura (la mina ya está desactivada en la suite); paginación de listados enormes (conecta con #27 y la cláusula del ADR 0004); MinIO testcontainers (nightly) | opendal es una dep GRANDE: justificar, feature-gate estricto, cargo-deny vigilante |
| 8 | `norte-vfs-archive` (read) | zip/tar como directorios virtuales de solo lectura; encoding de nombres ZIP: bit 11 UTF-8 o cp437/local, NUNCA a ciegas (trampa conocida); límites anti zip-bomb (threat model §14); copiar DESDE archive con el copy engine normal | encoding-auditor obligatorio; fixtures de nombres ZIP al corpus; RAR queda fuera (delegación, spec §16.5, M2+) |
| 9 | Papelera remota + deuda trash | Papelera lógica `.norte-trash/` para providers sin trash nativo (lo dejó apuntado el ADR 0009); revisar el recíproco N-1 del skew; #25 (Windows nuke warning) y #26 (cross-device freedesktop) | Extiende ADR 0009 o ADR nuevo |
| 10 | Calidad M2 | E2E del criterio de salida (cadena sftp → S3 → zip local, sin sorpresas = colisiones/cancelación/resume/nombres hostiles correctos en cada salto); job nightly de CI con testcontainers (sftp/MinIO) + fuzzing corto (nombres ZIP, framing JSON-RPC — spec §12); benchmarks de remotos; revisar #27 | El nightly NO entra en el gate de PR (flakiness de contenedores) |

## Decisiones a confirmar con el usuario en el kickoff

1. **Interpretación del criterio de salida**: archive es READ-ONLY en M2
   (spec §15) — la cadena verificable es sftp → S3 → local y LEER/copiar
   desde zip. ¿Escribir dentro de zip queda para M2+/M3 (propuesto: sí)?
2. **FTP plano**: el plan M1 lo dejó como "candidato a provider extra en
   M2+, decidir allí". Al usuario le importa ftp — ¿entra como fase 5bis
   (russh no lo cubre; sería dep aparte) o se difiere con issue?
3. **#27 (100k listado, 254 ms vs 200)**: ¿se ataca dentro de la fase 7
   (la paginación del protocolo es la mitigación grande) o se difiere?
4. **MessagePack** como encoding alternativo negociable (spec §11):
   ¿fase 2 lo deja negociado-pero-solo-JSON (propuesto) o entra ya?

## Riesgos principales

- **Daemon multiplataforma**: named pipes + SID en Windows es la parte
  oscura (UDS+SO_PEERCRED está trillado). Presupuestar tiempo de CI
  Windows; el modo embebido sigue existiendo como red de seguridad.
- **russh**: API async de bajo nivel y en evolución; aislar TODO detrás
  del trait `Provider` (nada de tipos russh en API pública).
- **opendal**: superficie de dependencias enorme — feature-gate mínimo,
  `cargo deny` y tamaño de build vigilados; alternativa (SDK aws directo)
  documentada en el ADR.
- **Resume ≠ offset en S3**: si el diseño de fase 4 asume POSIX, la fase
  7 lo rompe. El ADR de resume debe nacer con multipart en mente.
- **testcontainers en CI**: flaky por naturaleza → nightly separado del
  gate de PR desde el día 1 (la spec ya lo pide así, §12).
- **Secretos**: keyring headless (CI, servidores sin Secret Service) —
  fallback documentado a env vars, jamás a config plano.

## Lo que ya está listo (no rehacer)

- Trait `Provider` ENSANCHADO en M1-f2 pensando en esto: `read` con
  rango, symlinks, capabilities APPEND/RANDOM_WRITE,
  `ProviderUnavailable{retryable}` + backoff cancelable en el engine.
- `Authority` validada en VPath (`sftp://host:22/…`) desde M0.
- Colisiones contra el provider DESTINO; copy engine cross-provider
  genérico; `copy_native` con contrato anti-sobrescritura.
- `MemProvider` con desconexión/latencia/EIO inyectables (espejo local de
  los tests de remotos).
- Tipos de métodos `fs.*`/`task.*` en proto (0.3.0) con golden: la fase 2
  añade envelope y transporte, no reinventa params.
- TUI/CLI sin lógica de negocio (regla 7): apuntarlos al daemon es
  cambiar transporte, no reescribir frontends.
