# 0013 — Provider SFTP: russh, contención del servidor hostil y testing

- Estado: accepted
- Fecha: 2026-07-12
- Decisores: oscar (dirección), Claude (propuesta técnica)
- Relacionado: spec §3 (`norte-vfs-sftp`, russh), §4 (SFTP max 4 streams),
  §12 (testcontainers openssh), §14/threat model (servidor SFTP hostil
  `../../`), §15 (criterio de salida M2); ADR 0005 (contrato Provider),
  0012 (resume). Plan M2 fase 5.

## Contexto y problema

M2 estrena el primer provider REMOTO. Tres frentes con decisión:

1. **Qué librería** y cómo se aísla su API de bajo nivel del trait `Provider`.
2. **Contención de un servidor hostil**: la spec/threat-model exige tratar
   un servidor SFTP que devuelve nombres trampa (`../../`, symlinks fuera
   de la base) sin escapar la raíz ni corromper.
3. **Cómo se testea** sin depender de Docker en cada PR (el criterio de
   salida y la suite contractual deben correr en CI normal).

Y un cuarto, que emerge de la librería: **SFTP transporta nombres como
BYTES, pero russh-sftp los expone como `String` (UTF-8)** — choca con la
regla dura 1 (nombres = bytes).

## Opciones consideradas

### A. Librería SSH/SFTP

- **A1 — `ssh2` (libssh2, C)**: binding a C, build no-puro-Rust,
  cancelación pobre (API bloqueante). Descartada: rompe "nada de I/O
  bloqueante en async" sin `spawn_blocking` por operación.
- **A2 — `russh` 0.62 + `russh-sftp` 2.3 (Rust puro, async, la que la
  spec §3 nombra)**: transporte SSH async cancelable; `russh-sftp` da un
  `SftpSession` sobre cualquier stream. Contra: API en evolución (riesgo
  del plan) y `String` para paths (ver D). Dep GRANDE (aws-lc-rs,
  ~120 crates transitivos) — justificada: es EL protocolo remoto de M2.

### B. Frontera con el trait

- **B1 — `SftpProvider::connect(host, auth)` monolítico**: acopla el
  provider al establecimiento de la conexión SSH y a los secretos (fase 6),
  e impide testear el provider sin un servidor SSH real.
- **B2 — inyección de sesión**: `SftpProvider::new(session, base)` toma un
  `SftpSession` YA establecido; `connect(...)` (con auth) es un
  constructor aparte que llega en fase 6. Así la lógica del provider se
  testea contra un servidor sftp IN-PROCESS sobre un `duplex` (sin SSH),
  y el corpus hostil corre en CI normal.

### C. Testing

- **C1 — solo testcontainers openssh**: realista pero exige Docker en cada
  PR (flaky, lento) — la spec §12 pide el nightly separado del gate justo
  por esto.
- **C2 — servidor sftp IN-PROCESS + testcontainers nightly**: `russh-sftp`
  tiene lado servidor (`server::run(stream, handler)`) que corre sobre un
  stream crudo. Un servidor de test respaldado por un `tempdir` conectado
  al cliente por `tokio::io::duplex()` ejecuta la suite contractual
  COMPLETA (incluido el corpus hostil) en CI normal, sin Docker. El
  openssh real queda como job nightly (fuera del gate de PR).

### D. Nombres = bytes vs `String` de russh-sftp

- **D1 — decodificar lossy**: viola la regla 1 (un nombre no-UTF8 quedaría
  corrupto en silencio) — inaceptable.
- **D2 — UTF-8-only con rechazo LIMPIO (fail-loud en AMBAS direcciones)**:
  - *Escritura* (`VPath segment bytes → String`, en `remote()`/`symlink`): se
    convierte con `from_utf8`; un nombre no representable devuelve
    `Error::InvalidPath` (fail-loud, jamás lossy). Lo hace el propio provider.
  - *Lectura* (`String` del servidor → bytes del `Entry`): **russh-sftp ya
    decodificó el nombre con `from_utf8_lossy`** (su `try_get_string` tiene la
    variante estricta comentada), así que un byte no-UTF8 llega **sustituido
    por U+FFFD** antes de que el provider lo vea — los bytes originales se
    perdieron BAJO la frontera. Convertir ese `String` a bytes daría un `Entry`
    con bytes CORRUPTOS (colisión / path inexistente en `stat`/`read`/`copy`),
    no los reales. Por eso `list()`/`read_link()` **rechazan** limpio
    (`InvalidPath`) todo nombre/target que contenga U+FFFD, en vez de emitir
    bytes sustituidos. Es fail-loud, pero esos nombres **no son operables** con
    esta dependencia (limitación de russh-sftp, no del diseño — ver Negativas
    e issue #37).
  - El contrato ya tolera el rechazo (`contract_hostile_names_roundtrip` acepta
    `InvalidPath` como "el backend rechaza el nombre, limpio").

## Decisión

- **A2 + B2 + C2 + D2.**
- **Crate `norte-vfs-sftp`** (MIT/Apache; providers no se conocen entre sí).
  Deps: `russh`, `russh-sftp`, `tokio`, `bytes`, `futures`, `async-trait`,
  `norte-vfs`, `norte-proto`. `#![forbid(unsafe_code)]`.
- **`SftpProvider`** envuelve un `SftpSession` (Arc, compartido) enraizado
  en una `base` remota. La API de russh-sftp NO cruza la frontera pública:
  fuera del crate solo se ve el trait `Provider`.
- **Auth/secretos DIFERIDOS a fase 6**: aquí solo el transporte y la lógica
  del provider. `connect(addr, credentials)` (host key, user, key/agent)
  llega con `connections.toml` + keyring (ADR de fase 6).
- **Capabilities honestas**: `SYMLINKS` (sftp tiene symlink/readlink),
  `APPEND` + `RANDOM_WRITE` (open con offset/append — habilita el resume de
  ADR 0012 por append), `CASE_PRESERVING`. NO declara: `RENAME_ATOMIC`
  (el rename de sftp v3 no garantiza no-replace ni atomicidad), `TRASH`
  (sin papelera remota — llega en fase 9 con `.norte-trash/`),
  `SERVER_COPY` (no se asume la extensión copy-data). SÍ declara
  `CASE_SENSITIVE`: el remoto se asume POSIX (la abrumadora mayoría de
  servidores sftp), y declararla es el modo CONSERVADOR correcto — el
  engine trata los nombres como bytes exactos y no inventa colisiones de
  caja que un servidor Linux no tiene. Un remoto insensible (sftp sobre
  Windows) queda como refinamiento futuro (sondeo, como en `LocalProvider`).
- **`node_id` → `Ok(None)`**: las `FileAttributes` de sftp no llevan
  identidad estable (inodo). Consecuencia (ADR 0005/0012): los guards del
  engine degradan a heurística, y `Follow` sobre dir-symlinks responde
  `Unsupported` — que es justo la contención que queremos (ver abajo).
- **Contención del servidor hostil**:
  - El provider construye SIEMPRE los paths remotos a partir de `Segment`s
    ya validados del `VPath` bajo la `base`; JAMÁS usa un path absoluto
    ecoado por el servidor para otra operación.
  - Un nombre de `readdir` que contenga `/` o sea `.`/`..` se RECHAZA (un
    componente de path no puede llevar separador; un servidor que lo
    inyecte busca escapar la base): la entrada se descarta con error, el
    listado no continúa a ciegas.
  - `stat`/`node_id`/`read` usan `lstat` (describen el LINK, jamás lo
    siguen): un symlink trampa a `/etc/passwd` se ve como symlink, y como
    `node_id` es `None`, el `Follow` del engine no puede recorrerlo
    (`Unsupported`). El `read_link` devuelve los BYTES crudos del target
    (dato, no se resuelve). `read()` también RECHAZA un symlink
    (`TypeMismatch`): no lo sigue server-side, coherente con el invariante
    lstat — el engine recorre symlinks vía `read_link`, jamás vía `read`.
  - La `base` acota el árbol: los paths se componen `base + segmentos`, sin
    `..` (el `VPath` no los admite), así que el cliente nunca pide algo
    fuera de la base por su cuenta.
  - **Escritura sobre staging pre-plantado**: el nombre del staging es
    DETERMINISTA (`.norte-partial.eph.{seq}` en `write`, `.norte-partial.
    {fnv1a_128}` en `open_resumable`), así que un servidor/co-tenant hostil
    podría pre-plantarlo como symlink fuera de base y hacer que el `open`
    lo siga y escriba en el target (ataque symlink clásico de sftp/scp).
    Contención: `write` abre con `EXCLUDE` (SSH_FXF_EXCL = create-new
    atómico) → el `open` FALLA si el staging existe, jamás sigue el symlink,
    y de paso cierra el TOCTOU del staging. `open_resumable` no puede usar
    `EXCLUDE` (reabre un parcial legítimo): hace `lstat` del staging y
    RECHAZA (`TypeMismatch`) si es un symlink (TOCTOU documentada, misma
    clase que `check_final_absent`). Cubierto por
    `staging_symlink_pre_plantado_no_se_sigue_en_write` /
    `..._no_se_reanuda` y verificado contra OpenSSH real (nightly).
- **Concurrencia**: un `SftpSession` multiplexa; el límite "SFTP max 4
  streams" (spec §4) es del scheduler, no del provider (queda para cuando
  el scheduler tenga límites por-provider — issue).
- **Testing**: servidor sftp in-process (`server::run` sobre `duplex`,
  backend `tempdir`) para `provider_contract!` + corpus hostil en CI
  normal; un servidor de test con modo HOSTIL inyectable (readdir con
  `../../`, symlink fuera de base) para los tests de contención;
  testcontainers openssh en un job NIGHTLY (feature `it-openssh`, fuera del
  gate de PR por flakiness — spec §12): `.github/workflows/nightly.yml`
  corre `just it-remote` (imagen `atmoz/sftp`, OpenSSH real chrooteado)
  contra el provider — roundtrip write/stat/list/read y resume por append.
  `testcontainers` es dep opcional para no compilar su árbol en el gate; el
  gate usa features explícitas (`norte-tui/schema`), no `--all-features`.

## Consecuencias

Positivas:

- El corpus de nombres hostiles del testkit se lanza por primera vez contra
  un provider remoto DE VERDAD, en CI normal, sin Docker.
- La inyección de sesión desacopla la lógica del provider de la conexión
  SSH y de los secretos (fase 6): un solo punto que cambiar.
- La contención cae de forma natural del diseño: `lstat` + `node_id=None`
  cierran el recorrido de symlinks trampa sin código ad-hoc.

Negativas / deuda asumida:

- **Nombres UTF-8-only** por la API `String` de russh-sftp (ASIMETRÍA, ver
  D2): en *escritura* el provider rechaza `InvalidPath` fail-loud (jamás
  lossy). En *lectura* russh-sftp **ya decodificó lossy** (U+FFFD) antes de
  que el provider vea el nombre: no se puede recuperar los bytes originales,
  así que se rechaza fail-loud cualquier nombre/target con U+FFFD (mejor que
  emitir bytes corruptos que colisionarían o apuntarían a un path
  inexistente). Consecuencia: un nombre remoto no-UTF8 **no es operable**.
  El fix correcto —leer los bytes crudos del `SSH_FXP_NAME` (raw protocol o
  fork de russh-sftp)— es trabajo futuro (**issue #37**). Es una limitación
  de la DEP, no del diseño de norte.
- **macOS NFD**: el provider NO normaliza (preserva bytes, correcto). Pero un
  servidor sftp sobre macOS devuelve nombres en NFD; como `stat` reusa el
  `VPath` del cliente (NFC) y `list` usa el nombre del servidor (NFD), el
  mismo fichero puede verse con dos formas de bytes según la operación. La
  comparación en NFC es responsabilidad del ENGINE (trampa conocida de
  CLAUDE.md), no del provider. Se declara `CASE_SENSITIVE` asumiendo POSIX;
  contra un remoto insensible (Windows/macOS) el engine infra-predice
  colisiones de caja, pero `check_final_absent` (lstat) las reconvierte en
  `Conflict::Exists` en el write — llega tarde, no corrompe. Refinamiento por
  sondeo, como en `LocalProvider` (issue futura, no de esta fase).
- `russh`/`aws-lc-rs` es una superficie de dependencias grande
  (~120 crates): `cargo deny` vigilante, justificada en la PR (regla 8).
  `russh` (transporte SSH) es **dev-dependency** en fase 5 —solo lo usa el
  test nightly `openssh`; la lib habla sftp sobre una sesión ya establecida—,
  así que `rsa`/`aws-lc-rs` NO entran al build del gate hasta que fase 6
  cablee `connect()`.
  Además arrastra `rsa` (vía `ssh-key`) con **RUSTSEC-2023-0071** (Marvin,
  sidechannel de temporización en clave privada RSA) SIN fix upstream: se
  documenta en `deny.toml` `[advisories].ignore` con evaluación de riesgo
  (norte es cliente, no expone oráculo RSA; auth de cliente será ed25519 por
  defecto en fase 6) y se vigila en la issue #36.
- Sin `RENAME_ATOMIC` remoto, un rename same-provider en sftp usa el rename
  no-atómico del protocolo; el engine ya trata el `Conflict` por política
  (ADR 0005) y no depende de la atomicidad.
- El resume por append sobre sftp reabre en `O_APPEND`; si el servidor no
  respeta append (raro), el `already` reportado por `open_resumable`
  seguiría siendo la longitud del parcial, correcto.
- Auth y verificación de host key son fase 6: hasta entonces el provider no
  se conecta a servidores reales fuera de los tests. El nightly usa un
  helper de conexión de TEST (usuario/password conocidos, host key aceptada
  sin verificar) aislado tras `#[cfg(feature = "it-openssh")]` en `tests/`;
  el código de producción no tiene ningún `client::Handler`. Fase 6 traerá
  la verificación real (known_hosts/TOFU/keyring) y, para neutralizar
  RUSTSEC-2023-0071, rechazará explícitamente claves de cliente RSA
  (ed25519-only), no solo "por defecto".
