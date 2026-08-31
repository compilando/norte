# 0015 - Connections, secrets, TOFU, and Ed25519

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: ADRs 0007, 0011, 0013, 0014, and 0016; issues #36, #38, #320, #321,
  and #322

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

## Amendment, 2026-08-31: an empty secret is a failure, and the isolation claim was too wide

Two corrections, both found while configuring a real S3 account (#320, #321).

**An empty secret is now rejected, not resolved.** The Decision above says the
resolver returns the secret from env, keyring or `secrets.age`; it did not say
what an *empty* value means. It meant nothing, and that was the bug: opendal's
`secret_access_key` setter silently discards an empty string (`if
!v.is_empty()`), so no static credential provider was registered and the
connection authenticated with whatever the ambient chain offered — on a
developer machine, an expired SSO profile; on a machine with a valid role, the
wrong identity, without a word. `SecretResolver::resolve` now fails with
`ConnectError::SecretEmpty`, naming the connection and which step held the
empty value, and `S3Connector::connect` repeats the check on both halves of the
credential — an empty `access_key_id` gates the same static provider and
reproduces the bug on its own. An empty `endpoint` or `region` is refused for
the same reason: opendal discards them and silently retargets the request at
AWS. A whitespace-only secret is deliberately NOT rejected: it reaches the
server and dies with an actionable credential rejection.

Two limits of that, both deliberate. The rejection does NOT apply to
`auth = "key"`, where the secret is a key passphrase and empty means the same
as absent, with no ambient credential behind it to substitute — the core's
`establish` unwinds it for that method alone. And the sentence naming the step
reaches the daemon log and `norte doctor`, not the frontend: `norte_proto`
collapses every credential failure into `PermissionDenied`, and nothing on this
path fills the RPC `message`. A TUI user still reads "permission denied". #322
tracks carrying the detail across the wire; until it lands, `norte doctor` is
where the diagnosis lives, which is why the set-but-unusable cases there are
`Error` rather than `Warn`.

**The isolation claim is narrower than written.** "Explicit access-key
authentication … disables ambient configuration/IMDS" describes intent, not
what opendal 0.58 delivers: `disable_config_load` is `no_env()` + `no_profile()`
and `disable_ec2_metadata` is `no_imds()`, leaving SSO, web-identity, process
and ECS providers in the chain, with no builder flag reaching reqsign's own
`no_sso()`/`no_web_identity()`/`no_process()`/`no_ecs()`. What actually holds
the determinism is ordering: the static provider is pushed to the FRONT of the
chain and wins whenever explicit credentials exist. The chain is consulted only
when they do not — which is precisely the hole the first correction closes.
#321 tracks the upstream ask.
