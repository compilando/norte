# 0001 — Representación de VPath y su wire format

- Estado: accepted
- Fecha: 2026-07-08
- Decisores: Oscar González (elección de wire), Claude (sesión M0)

## Contexto y problema

Los nombres de archivo NO son UTF-8 (principio 3 de la spec): Linux permite
bytes arbitrarios salvo `/` y NUL; Windows es UTF-16 con posibles surrogates
sin parear; macOS normaliza a NFD. `VPath` debe (a) representar cualquier
nombre sin pérdida, (b) viajar por JSON-RPC — y JSON exige strings UTF-8
válidos — y (c) hacer el roundtrip byte-exacto siempre.

## Opciones consideradas

1. **Interno bytes + wire percent-encoding (RFC 3986-style)**
   - ✓ Un solo string en el wire, coherente con la forma URI `scheme://…`.
   - ✓ Legible en logs, fixtures y debugging (el caso común, UTF-8, va tal cual).
   - ✓ Roundtrip byte-exacto demostrable con proptest.
   - ✗ Reglas de escape que hay que testear (`%` literal → `%25`, escapes malformados).
2. **Interno bytes + wire base64-tagged por segmento**
   - ✓ Parsing trivial, sin reglas de escape.
   - ✗ El VPath deja de ser un string único (pasa a array/objeto); paths ilegibles
     en logs; fixtures opacas.
3. **WTF-8 directo como string JSON**
   - ✗ Inviable: JSON exige UTF-8 válido; surrogates sin parear y bytes arbitrarios
     no sobreviven a serde_json (rechazo o sustitución por U+FFFD → roundtrip roto).
     Solo funcionaría con encoding binario (MessagePack), que es opcional en el
     protocolo, no la base.

## Decisión

**Opción 1**, elegida por el usuario en sesión (2026-07-08).

- **Interno:** `VPath { scheme, authority: Option<Authority>, segments: Vec<Segment> }`
  con `Segment(Vec<u8>)` — bytes crudos. En Unix, los bytes del OS tal cual; en
  Windows, la forma WTF-8 que expone `OsStr::as_encoded_bytes()` (roundtrip de
  surrogates garantizado por el propio `OsString`).
- **Invariantes** (validadas en construcción, no saneadas en silencio): segmento
  no vacío, sin NUL, sin `/`, sin `.` ni `..` literales; scheme `[a-z][a-z0-9+.-]*`.
- **Authority validada:** `Authority(String)` — ASCII imprimible (0x21–0x7E)
  sin `/` ni `%`, nunca vacía (`Some("")` es inconstruible; la ausencia es
  `None`). La authority NO lleva percent-encoding: viaja literal, y como no
  puede contener `/`, el primer `/` tras `://` separa siempre authority de
  path — el wire es inyectivo. Hosts no-ASCII van en punycode (decisión del
  provider, fuera de proto).
- **Wire:** string único `scheme://authority/seg1/seg2`. Encoding por segmento:
  secuencias UTF-8 válidas van literales; cualquier byte fuera de una secuencia
  UTF-8 válida → `%XX`; `%` literal → `%25`; controles C0 (0x00–0x1F) y DEL
  (0x7F) → `%XX` aunque sean UTF-8 válido — un wire jamás lleva bytes de
  control crudos (terminal injection y log forging vía nombres de archivo).
  Decode: `%XX` → byte crudo; escape malformado → error (nunca pérdida
  silenciosa). El decode acepta formas no canónicas (`%41` ≡ `A`): el roundtrip
  garantizado es bytes → wire → bytes.
- **Aislamiento:** el codec vive en `norte-proto/src/wire/vpath_codec.rs`; nada
  fuera de ese módulo conoce las reglas de escape.
- **Display:** `display_lossy()` tiene forma propia `⟨scheme authority⟩/seg/…`,
  deliberadamente NO parseable (sin `://`): `parse(display)` falla siempre, así
  que un display jamás reconstruye un path por accidente. UTF-8 lossy con `�`
  marcando tanto bytes no decodificables como caracteres de control (nada de
  controles crudos hacia un terminal).
- La conversión `VPath ↔ PathBuf` NO vive en proto: es `native_path` en
  `norte-vfs-local` (confina el `unsafe` de `from_encoded_bytes_unchecked` y los
  `cfg` por OS). **Frontera Windows:** `as_encoded_bytes()` solo garantiza WTF-8
  para bytes nacidos de un `OsString` local; bytes llegados por red (un VPath
  decodificado del wire) DEBEN validarse como WTF-8 en esa frontera antes de
  cualquier `from_encoded_bytes_unchecked` — usar unchecked sobre input de red
  es unsound.

## Consecuencias

- ＋ Frontends y agentes ven paths legibles en el caso común y nunca corrompen
  el raro; golden tests y proptest fijan el formato desde el primer tipo.
- ＋ MessagePack (futuro, §11) puede llevar los bytes crudos sin tocar este diseño.
- － Todo productor/consumidor del protocolo debe usar el codec (documentado en
  el rustdoc de `norte-proto`); un cliente naïf que concatene strings puede
  generar escapes inválidos — el core los rechaza con error tipado.
- － `%` en nombres de archivo reales (raro pero legal) siempre viaja escapado.
