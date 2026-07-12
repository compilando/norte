# 0015 — Conexiones y secretos: connections.toml, keyring, TOFU, ed25519

- Estado: accepted
- Fecha: 2026-07-12
- Decisores: oscar (dirección + 4 decisiones de seguridad), Claude (propuesta)
- Relacionado: spec §13 (`connections.toml`, keyring, `zeroize`), §14 (threat
  model), regla dura 10 (secretos jamás en config/logs); ADR 0007 (config en
  capas), 0011 (daemon), 0013 (sftp — `connect` diferido a esta fase), 0014
  (ftp — FTPS/opt-in diferido). Plan M2 fase 6. Issues #36 (RSA/Marvin),
  #38 (FTPS/cleartext). **security-reviewer OBLIGATORIO** (plan M2).

## Contexto y problema

Las fases 5/5bis dejaron los providers sftp/ftp con **inyección de sesión**
(`new(session, base)`): la conexión con auth se difirió aquí. Fase 6 la cablea,
y con ella el manejo de SECRETOS — el terreno más sensible de M2:

1. **Dónde vive el establecimiento de conexión** (que toca secretos) sin
   romper la regla 7 (providers sin lógica de negocio) ni la 10 (secretos).
2. **Formato de `connections.toml`**: referencias, nunca secretos.
3. **De dónde salen los secretos**: keyring del OS, pero CI/servidores headless
   no tienen Secret Service/Keychain.
4. **Verificación de host key** (sftp): sin ella, MITM trivial.
5. **Claves de cliente**: la advisory RUSTSEC-2023-0071 (Marvin) afecta a la
   firma RSA (issue #36).
6. **FTP**: cleartext salvo FTPS (issue #38).

## Decisiones (4 cerradas por oscar)

### A. Arquitectura: el "connection manager" vive en el CORE

El establecimiento de conexión (leer `connections.toml`, resolver el secreto,
abrir SSH/FTP con auth, verificar host key) vive en **`norte-core`** (AGPL), no
en los crates de provider (MIT/Apache, secret-agnósticos por diseño — ADR
0013/0014 B2). El core:

1. resuelve la conexión por nombre o URL ad-hoc → parámetros;
2. resuelve el secreto (orden en C);
3. establece el transporte (russh para sftp: verifica host key + auth;
   suppaftp para ftp: AUTH TLS + login);
4. construye `SftpProvider::new(session, base)` / `FtpProvider::new(...)` con la
   sesión YA establecida.

Así los providers NUNCA ven un secreto ni el keyring (regla 7 + 10). Un nuevo
subsistema `norte-core::connect` (o crate `norte-connect` si crece) encapsula
russh-client/suppaftp-connect + keyring + known_hosts. **Los tipos de auth de
russh/suppaftp NO cruzan al resto del core.**

### B. `connections.toml` — SOLO referencias (regla 10)

```toml
[connections.trabajo]
url = "sftp://oscar@sftp.example.com:22"   # scheme+user+host+port
auth = "key"                                # "key" | "password" | "agent"
key = "~/.ssh/id_ed25519"                   # ruta a la clave (no el secreto)
# El passphrase/password NO va aquí: se resuelve del keyring/env/fichero (C).

[connections.s3backup]
url = "ftp://backup@ftp.example.com:21"
auth = "password"
tls = "require"                             # "require" (FTPS) | "allow" | "plain"
```

`connections.toml` es una capa más del sistema de config (ADR 0007). NUNCA
contiene secretos; solo la referencia (`url` + `auth` + ruta de clave / política
TLS). El passphrase de una clave o el password se resuelven aparte.

### C. Secretos: keyring del OS + env vars + fichero cifrado (decisión oscar)

Orden de resolución (primero que acierte gana):

1. **Env var** `NORTE_SECRET_<CONN>` (mayúsculas, `-`/`.`→`_`): override
   explícito para CI/corporativo (spec §13). Precede a todo — un CI puede fijar
   el secreto sin tocar el keyring.
2. **Keyring del OS** (`keyring` crate v3: Secret Service / Keychain /
   Credential Manager), servicio `norte`, cuenta = la `url` de la conexión.
   Default en escritorio.
3. **Fichero cifrado** `secrets.age` en el dir de config (decisión oscar: para
   headless PERSISTENTE sin Secret Service — servidores). Cifrado con `age`
   (X25519 o passphrase); la clave/passphrase de `age` sale de env
   (`NORTE_SECRETS_KEY`) o de un fichero con permisos 0600. Nunca en claro.

El secreto en memoria se envuelve en `zeroize` (spec §265) y se borra tras usar.
Los secretos JAMÁS se loguean (ni redactados con longitud — solo "presente/
ausente"). `connections.toml` y los logs pasan el mismo lint que el daemon.

Dep nueva: `keyring` (v3, cross-platform, el estándar) + `age`/`rage` (cifrado
del fichero; auditado, moderno) + `zeroize`. Justificadas en la PR (regla 8);
`cargo deny` vigilante.

### D. Host key SSH: TOFU + known_hosts propio (decisión oscar)

Trust-on-first-use como OpenSSH: un `known_hosts` propio en el dir de config.

- **Primera conexión a un host desconocido**: el core NO acepta a ciegas.
  Devuelve un error tipado `HostKeyUnknown { fingerprint, algo }`; el frontend
  MUESTRA el fingerprint y pide confirmación; un `connections.trust_host_key`
  explícito lo registra en `known_hosts`. (Requiere un método de protocolo para
  la confirmación — **cambio de wire → golden + bump + protocol-guardian**; se
  detalla en la implementación. Alternativa para CI: `NORTE_KNOWN_HOSTS` o
  pre-poblar el fichero.)
- **Conexiones siguientes**: verificación ESTRICTA contra `known_hosts`. Una
  host key que CAMBIA → error `HostKeyMismatch` (posible MITM), jamás se acepta
  en silencio.
- El `check_server_key` de russh (que hoy en el test devuelve `Ok(true)`) se
  reemplaza por esta verificación real.

### E. Claves de cliente: ed25519-only (decisión oscar, cierra #36)

La auth por clave de cliente **rechaza RSA** explícitamente; solo ed25519 (y
ecdsa como refinamiento). Neutraliza el vector Marvin (RUSTSEC-2023-0071) de
raíz: `rsa` sigue en el árbol (transitivo de ssh-key) pero NO se usa para firmar
con clave de cliente. Un usuario con solo clave RSA recibe un error claro que
recomienda ed25519. Se documenta y se cierra la deducción de #36.

### F. FTPS (cierra parte de #38)

`tls = "require"` (default recomendado) exige AUTH TLS (suppaftp feature
`async-secure` + rustls); `"plain"` es opt-in EXPLÍCITO con aviso de inseguro;
`"allow"` intenta TLS y cae a plano con aviso. FTP plano nunca es silencioso.

### G. UX mínima (spec / plan)

CLI: `norte ls sftp://host/path` resuelve por `connections.toml` (match por
host/user) o pide credenciales; `norte connect <nombre>`. TUI: abrir una ruta
remota dispara el flujo (incluida la confirmación TOFU). Mínimo viable en fase
6; el gestor de conexiones rico es posterior.

## Consecuencias

Positivas:
- La inyección de sesión de 0013/0014 encaja: fase 6 solo añade el "cómo se
  establece", sin tocar la lógica de los providers.
- Secretos centralizados en el core con una sola ruta de resolución auditable.
- TOFU + ed25519-only + FTPS cierran los vectores abiertos (#36, #38, MITM).

Negativas / deuda asumida:
- **Superficie de deps nueva** (keyring, age, zeroize, rustls para FTPS):
  `cargo deny` + justificación. El fichero `age` añade cripto propia de
  almacenamiento — acotada al store de secretos.
- **Cambio de protocolo** para la confirmación TOFU (host key desconocida →
  frontend): golden + bump + protocol-guardian en la implementación.
- **russh sale de dev-dep a dep normal** en `norte-vfs-sftp`? No: el `connect`
  vive en el CORE, que ya es AGPL y puede depender de russh directamente; el
  provider sigue recibiendo la sesión inyectada. (Confirmar en implementación:
  si el core construye el `SftpSession`, russh es dep del core, no del provider.)
- El fichero `secrets.age` es deuda de UX (gestión de la clave de `age`);
  documentar bien o dejar env-var como camino principal en CI.

## Plan de implementación (fase 6, con security-reviewer OBLIGATORIO)

1. ADR (este) — HECHO.
2. Crate/módulo `norte-core::connect`: modelo `ConnectionSpec` (parse de
   `connections.toml`), `SecretResolver` (env→keyring→age, con `zeroize`).
3. Transporte sftp: `connect_ssh(spec, secret)` con russh (host-key TOFU real,
   ed25519-only) → `SftpSession` → `SftpProvider`.
4. Transporte ftp: `connect_ftp(spec, secret)` con suppaftp (FTPS por `tls`) →
   `FtpProvider`.
5. Proto: método de confirmación de host key (golden + bump + protocol-guardian).
6. `known_hosts` store + `secrets.age` store (zeroize, permisos 0600).
7. CLI/TUI: UX mínima + flujo TOFU.
8. Tests: unit (resolver, parse, known_hosts), integración (nightly: connect
   real a atmoz/sftp con clave ed25519 conocida + pure-ftpd con FTPS).
9. Cierre: **security-reviewer (obligatorio)** + rust + protocol-guardian +
   encoding (si toca nombres) + `just ci` + commit + CI + memoria.
