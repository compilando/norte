# 0015 - Connections, secrets, TOFU, and Ed25519

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: ADRs 0007, 0011, 0013, 0014, and 0016; issues #36 and #38

## Context

SFTP and FTP providers accept established sessions so their logic remains
independent from credentials. The core now needs to parse connection profiles,
resolve secrets on desktops and headless hosts, verify SSH host keys, avoid the
RSA Marvin vulnerability, and make cleartext FTP an explicit choice.

## Decision

### Connection ownership

Connection establishment belongs to the core connection subsystem, not the
permissively licensed providers. It resolves a named or ad-hoc connection,
obtains its secret, authenticates and verifies the transport, then injects the
established session or OpenDAL operator into the provider. Third-party library
authentication types do not escape this subsystem.

### Connection profiles

`connections.toml` contains references and public parameters only:

```toml
[connections.work]
url = "sftp://user@sftp.example.com:22"
auth = "key"
key = "~/.ssh/id_ed25519"

[connections.backup]
url = "ftp://backup@ftp.example.com:21"
auth = "password"
tls = "require"

[connections.storage]
url = "s3://my-bucket"
auth = "access-key"
access_key_id = "AKIAEXAMPLE"
region = "eu-west-1"
endpoint = "https://minio.internal:9000"
addressing = "path"
```

Passwords, private-key passphrases, and S3 secret access keys never appear in
this file.

### Secret resolution

Resolve the first available source in this order:

1. `NORTE_SECRET_<CONNECTION>`, normalized to uppercase with punctuation
   replaced by underscores, for CI and explicit overrides.
2. The operating-system keyring under service `norte` and the connection URL.
3. Encrypted `secrets.age` in the configuration directory for persistent
   headless systems. Its age identity or passphrase comes from
   `NORTE_SECRETS_KEY` or a mode-0600 file.

Wrap in-memory secrets with zeroization and erase them after use. Logs record
only presence or absence, never a value or its length.

### SSH trust and client keys

Maintain a norte-specific `known_hosts` file. An unknown host key returns a
typed error containing its algorithm and fingerprint. A frontend presents it
and an explicit trust operation records it. Later connections require an exact
match; changes return `HostKeyMismatch` and are never silently accepted. CI can
prepopulate the file.

Reject RSA client keys and recommend Ed25519, avoiding the
RUSTSEC-2023-0071 signing path. ECDSA may be added later.

### FTP and object-storage channels

For FTP, `tls=require` enforces explicit FTPS, `plain` is a warned opt-in, and
`allow` attempts TLS before a warned fallback.

For S3, inject an OpenDAL operator built by the connection layer. Explicit
access-key authentication combines a public ID from configuration with a
secret from the resolver and disables ambient configuration/IMDS. `auth=agent`
uses the ambient AWS chain for CI or instance roles. A custom HTTP endpoint is
accepted only as a visible opt-in warning. A custom endpoint using ambient
credentials must come from operator-controlled configuration, preventing an
ad-hoc agent URL from becoming an IMDS credential-forwarding target. Standard
S3 uses TLS/WebPKI rather than TOFU.

## Consequences

Providers remain secret-agnostic and all credentials pass through one auditable
path. TOFU, Ed25519-only client authentication, and FTPS close the immediate
MITM, Marvin, and cleartext risks. Keyring, age, zeroize, and TLS add structural
dependencies and the encrypted-file path needs careful key-management UX.

Host-key trust requires versioned protocol methods and golden tests. The core,
not the provider crate, owns Russh client dependencies. Minimum CLI/TUI UX must
support named connections, remote paths, and the trust confirmation flow.
