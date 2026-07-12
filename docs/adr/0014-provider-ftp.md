# 0014 — Provider FTP: suppaftp, MLSD, testing in-process y cleartext

- Estado: accepted
- Fecha: 2026-07-12
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §3 (providers de primera parte); ADR 0005 (contrato
  Provider), 0012 (resume), 0013 (provider sftp — mismo patrón). Plan M2
  fase 5bis (FTP plano, elección del usuario contra la recomendación de
  diferir; crate/dep aparte que REUTILIZA la suite contractual de sftp).

## Contexto y problema

FTP es el segundo provider remoto. Comparte forma con sftp (ADR 0013) pero
con diferencias propias del protocolo:

1. **Qué librería** cliente async y cómo se aísla del trait `Provider`.
2. **FTP es un protocolo de DOS conexiones** (control + datos) y una conexión
   de control ESTATAL (CWD, TYPE, REST se acumulan): no multiplexa como un
   `SftpSession`.
3. **Listado**: FTP clásico (`LIST`) devuelve texto `ls -l`/DOS heurístico;
   `MLSD` (RFC 3659) es machine-readable (`facts;nombre`). ¿Cuál?
4. **Encoding de nombres**: FTP no tiene encoding estándar (RFC 2640 hace
   UTF-8 OPCIONAL); choca con la regla dura 1 (nombres = bytes).
5. **Modo de transferencia**: FTP arranca en ASCII, que CORROMPE binarios.
6. **Seguridad**: FTP plano manda credenciales Y datos en claro.
7. **Cómo se testea** sin Docker en cada PR.

## Opciones consideradas

### A. Librería cliente

- **A1 — `async-ftp`**: sin mantenimiento activo.
- **A2 — `suppaftp` 10 (feature `tokio`)**: fork mantenido de rust-ftp,
  cliente async sobre tokio, MLSD/MLST + `resume_transfer` (REST) + APPE, y
  `connect_with_stream(TcpStream)` que permite inyectar un stream ya
  establecido (base del test in-process, igual que sftp). ELEGIDA.

### B. Frontera con el trait

- **B1 — `connect(host, creds)` monolítico**: acopla a la conexión y a los
  secretos (fase 6), e impide testear sin servidor real.
- **B2 — inyección de conexión**: `FtpProvider::new(stream, base)` toma un
  `AsyncFtpStream` YA conectado y logueado; `connect(addr, creds)` (auth
  cleartext / FTPS) llega en fase 6. Como la conexión de control es ESTATAL y
  `&mut`, el provider la envuelve en `Arc<Mutex<…>>` (mutex async) y serializa
  las operaciones — inherente a FTP (una sola conexión de control), no un
  defecto. ELEGIDA.

### C. Listado: LIST vs MLSD

- **C1 — `LIST` (`ls -l`/DOS)**: parsing heurístico y frágil (formatos por
  servidor, locale en fechas, nombres con espacios).
- **C2 — `MLSD` (RFC 3659) con respaldo `LIST` (dual-path)**:
  `type=file;size=…;modify=… nombre`, machine-readable y sin ambigüedad, robusto
  con nombres hostiles (espacios, saltos, control); `MLST` para un solo nodo
  (stat). PERO **MLSD/MLST NO es universal**: el nightly reveló que pure-ftpd
  (`delfer/alpine-ftp-server`, uno de los servidores más comunes) NO lo anuncia
  en `FEAT` (solo `MDTM/SIZE/REST/PASV/UTF8`). Por eso el provider SONDEA
  `FEAT` al construir (`has_mlsd`) y ramifica: con MLSD → `MLST`/`MLSD` directos
  (robusto); sin MLSD → `LIST` (`ls -l`, universal) del directorio PADRE +
  búsqueda por nombre para `stat`, `LIST` del propio dir para `list`. El
  parsing de `ls -l` es frágil con nombres hostiles (por eso el corpus hostil
  del contrato corre contra libunftp, que SÍ tiene MLSD); un servidor sin MLSD
  sirve nombres simples. ELEGIDA.

### D. Nombres = bytes vs `String` de suppaftp

Igual que sftp (ADR 0013 D2): suppaftp expone nombres como `String` y decodifica
la conexión de datos con `String::from_utf8_lossy` (tokio_ftp.rs). Un nombre no
representable llega ya sustituido por U+FFFD bajo la frontera.

- **D1 — decodificar lossy**: viola la regla 1 (corrupción silenciosa).
- **D2 — UTF-8-only con rechazo LIMPIO fail-loud, en AMBAS direcciones**:
  escritura `VPath bytes → String` con `from_utf8` (`InvalidPath` si no cabe);
  lectura rechaza `InvalidPath` cualquier nombre con U+FFFD (no se pueden
  recuperar los bytes). Además se envía `OPTS UTF8 ON` si `FEAT` anuncia UTF8,
  para que los servidores que lo soportan sí devuelvan UTF-8 real. Deuda del
  fix de bytes crudos compartida con sftp (issue #37). ELEGIDA.

## Decisión

- **A2 + B2 + C2 + D2.** Crate `norte-vfs-ftp` (MIT/Apache; providers no se
  conocen entre sí). Deps: `suppaftp` (feature `tokio`), `tokio`, `bytes`,
  `futures`, `async-trait`, `norte-vfs`, `norte-proto`. `#![forbid(unsafe_code)]`.
- **`FtpProvider`** envuelve `Arc<Mutex<AsyncFtpStream>>` enraizado en una
  `base` remota. La API de suppaftp NO cruza la frontera pública.
- **Modo BINARIO obligatorio**: al construir el provider se fija
  `transfer_type(FileType::Binary)` (`TYPE I`) — FTP arranca en ASCII, que
  traduce CRLF y CORROMPE binarios. Es la trampa #1 de FTP.
- **Auth/secretos DIFERIDOS a fase 6**: aquí solo el transporte y la lógica.
- **Capabilities honestas**: `APPEND` (APPE + REST habilitan el resume de ADR
  0012), `CASE_PRESERVING`, `CASE_SENSITIVE` (POSIX conservador, como sftp). NO
  declara: `SYMLINKS` (FTP base no crea symlinks), `RANDOM_WRITE` (REST+STOR no
  es fiable entre servidores), `RENAME_ATOMIC` (RNFR/RNTO no atómico),
  `TRASH`, `SERVER_COPY`. `node_id → Ok(None)` (FTP no da inodo — corta el
  Follow del engine, como sftp).
- **Resume (ADR 0012)**: `open_resumable` usa `SIZE` del parcial + `REST`/APPE
  para reanudar; `read` con rango usa `REST offset` antes de `RETR`.
- **Contención del servidor hostil**: idéntica a sftp — paths SIEMPRE desde
  segmentos validados del `VPath` bajo la `base`, nunca un path ecoado; un
  nombre de MLSD/LIST con `/`, `.`, `..` o U+FFFD se RECHAZA (`InvalidPath`) y
  el listado no continúa a ciegas.
- **Inyección CRLF (específica de FTP)**: FTP es un protocolo de LÍNEAS (el
  comando termina en CRLF). Un nombre con `\r`/`\n` inyectaría un comando FTP
  arbitrario (`STOR x\r\nDELE víctima`). `Segment` admite CR/LF (son nombres
  válidos en POSIX), así que `remote()` los RECHAZA (`InvalidPath`) antes de
  componer cualquier comando. sftp (binario, con framing) es inmune a esto. Sin symlinks que seguir, la superficie es
  menor que sftp. El parsing MLSD→nombre→`Segment` se aísla en una función
  testeable con inputs hostiles (no hace falta un servidor FTP mentiroso).
- **Seguridad — cleartext**: FTP plano manda usuario/contraseña Y datos en
  CLARO (regla 10: los secretos no van en logs ni config — el keyring de fase
  6 aplica igual, pero el transporte los expone en la red). La mitigación es
  **FTPS** (AUTH TLS explícito, `into_secure`), que llega en fase 6; hasta
  entonces el provider NO se conecta a servidores reales fuera de los tests.
  Fase 6 exigirá opt-in explícito para FTP plano (aviso de que es inseguro) y
  FTPS por defecto donde el servidor lo soporte. Se registra en la issue #38 (fase 6).
- **Testing**: servidor FTP IN-PROCESS con `libunftp` + `unftp-sbe-fs`
  (dev-deps) bound a `127.0.0.1:0` (puerto efímero) respaldado por un `tempdir`,
  en una task tokio; el cliente conecta por TCP de localhost (FTP exige sockets
  reales — control + datos PASV —, no vale un `duplex`). Corre `provider_
  contract!` sin Docker. Solo-Linux (como sftp): el harness se respalda en el
  FS del host y solo es fiel en POSIX. Los tests de contención del parsing van
  aparte (unit, con nombres hostiles). Nightly (feature `it-ftp`, fuera del
  gate): servidor FTP REAL `delfer/alpine-ftp-server` por testcontainers
  (la misma imagen que usa suppaftp) — roundtrip + resume.

## Consecuencias

Positivas:

- Segundo provider remoto reutilizando el contrato, el patrón de inyección y el
  de servidor in-process de sftp: coste marginal bajo.
- MLSD da metadatos fiables sin heurística de `ls -l`.
- Sin symlinks, la contención es más simple que en sftp.

Negativas / deuda asumida:

- **Nombres UTF-8-only** (misma asimetría que sftp, D2): fail-loud con U+FFFD
  en lectura, fix de bytes crudos = issue #37 (compartida).
- **Conexión de control ÚNICA y estatal**: `Arc<Mutex>` serializa las
  operaciones del provider — no hay paralelismo dentro de una conexión (el
  scheduler abre varias conexiones si hace falta — futuro).
- **Cleartext**: sin FTPS (fase 6) el provider es inseguro en red real; por eso
  no se conecta fuera de los tests hasta fase 6. FTP es candidato #1 a migrar a
  plugin-provider en M4 (issue #30).
- `suppaftp` arrastra `chrono` (fechas MLSD/MDTM) y, con TLS, `native-tls`/
  `rustls` (no se activan en fase 5bis: sin feature secure). `cargo deny`
  vigilante, deps justificadas en la PR (regla 8).
- MLSD no está garantizado en todo servidor; el degradado a `LIST` es
  heurístico (parsing POSIX/DOS de suppaftp) — aceptable, con `size`/`mdtm` de
  respaldo.
