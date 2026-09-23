# 0015 - Connections, secrets, TOFU, and Ed25519

- Status: accepted
- Date: 2026-07-12
- Decision makers: Oscar González
- Related: ADRs 0007, 0011, 0013, 0014, and 0016; issues #36, #38, #320, #321,
  #322, #325, and #327

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
secret = "prompt"   # ask if env/keyring/age have nothing (2026-09-01 amendment)

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

> **Amended by [ADR 0150](0150-rsa-client-keys-as-a-per-connection-opt-in.md):**
> a connection may accept an RSA key file with an explicit `allow_rsa = true`
> (rsa-sha2 only, warned on every use). Without it, RSA is still rejected.

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

## Amendment, 2026-09-01: a fourth source that ASKS, and what crosses the socket

The Decision above lists three sources and stops. When none of them has the
secret, the connection fails — right on a CI runner, wrong on a laptop, where
the person who knows the password is sitting in front of the screen and norte
has no way to take it from them. #325 adds the fourth step.

**It is opt-in, per connection, and last.** A `secret = "prompt"` key on the
entry (`SecretSource::{Stored, Prompt}`, default `Stored`) says "and if the
three above come up empty, ask". Last and not first so the CI runner with the
variable set never sees a dialog, and opt-in so a headless daemon does not
start blocking on a question nobody will answer. An AD-HOC connection — a URL
typed into a pane, with no entry in `connections.toml` — never prompts: there
is no entry to declare it, and prompting for a typed URL would teach people to
type passwords into whatever dialog appears.

It applies to `auth = "password"` and `auth = "access-key"` only. With `agent`
there is no secret to ask for; with `key` the secret is the key's passphrase,
where empty and absent mean the same thing, so prompting would pop a dialog
every time anyone uses an unencrypted key. A key set where it does nothing is
not silently ignored — `norte doctor` reports `conn-secret-prompt-inert`.

**The dialog names the endpoint, and that is not decoration.** `SecretNeeded`
carries `conn` *and* `scheme://host[:port]`, redacted of userinfo the way
`ConnectionDegraded` redacts its host. A password dialog that says only
"connection: work" cannot be answered with any judgement: the name was chosen
by `connections.toml`, which can arrive from someone else's dotfiles or a
single edited line, and "work" says nothing about whether that entry still
points where it did yesterday. It is the same reason the host-key dialog shows
the fingerprint — the human verifies the *counterparty*, not a local label.
The risk is not uniform across schemes, which is why the endpoint is mandatory
rather than nice-to-have: an `sftp` still passes the host-key TOFU before
anything is sent, but `ftp` goes out in the clear and an `s3` with an
attacker-chosen `endpoint` signs a request against the server that entry picked.

**The mechanism is the TOFU flow, reused.** `Error::SecretNeeded { conn }`
(protocol 0.63.0) suspends the navigation exactly as `HostKeyUnknown` does; the
frontend opens a modal; `connection.provide_secret` carries the answer back;
that same navigation is retried. Like `connection.trust_host_key`, the daemon
rejects the method for any actor that is not `Actor::User` with
`INVALID_REQUEST` — an agent that could inject session credentials would be
choosing which identity the user acts under on the remote host — and there is
no journal entry, because nothing in the file tree changed and there is nothing
to undo.

Two limits of that gate, stated so nobody reads it as more than it is. `Actor`
is **declared** at `initialize`, not authenticated; any same-uid process can
claim `User`. That is the pre-existing model — `trust_host_key` rests on the
same thing — and the socket's 0600 mode plus the `SO_PEERCRED` check are what
actually keep other users out. And the gate covers **injection, not use**: an
agent cannot supply a secret, but an agent session on the same daemon can use
a connection a human unlocked, exactly as it could already use one unlocked by
an environment variable.

**What is stored, for how long, and how it is undone.** `SecretResolver` gains
an in-memory map consulted as step 0. Precisely: **one map per daemon process,
shared by every client of that uid**, not one per frontend — it outlives the
`ntc` that answered by up to the daemon's idle timeout, so a second frontend
started inside that window inherits an unlocked connection it never authorised.
Nothing is written to `connections.toml`, the keyring, or `secrets.age`;
stopping the daemon discards it. Offering to remember it is a separate decision
and deliberately not taken here: on Linux the keyring backend is not even
compiled in (`linux-keyring` is an opt-in feature nobody enables), and
`secrets.age` has no write path from a frontend.

Because step 0 sits ahead of the other three, a wrong value is worse than a
failure: it shadows the environment variable someone would reach for to fix it,
and the core only asks when it finds *nothing*, so the dialog would never come
back. So `establish` **forgets** a session secret the server rejects and turns
the failure back into `SecretNeeded` — the dialog reopens. Only the session
rung: a secret in the environment, the keyring or the age file was put there
deliberately somewhere editable, and deleting it on a server's say-so would be
deciding for its owner. The eviction is conditioned on the origin of the
credential that actually failed, not merely on a rejection having happened;
without that, an `auth = "key"` with a bad key path — also `PermissionDenied` —
would throw away an unrelated password and ask for it again.

**The secret crosses the socket in cleartext, and that is accepted.** The UDS
socket is mode 0600 and owned by the user (the daemon also refuses to run as
root, hardens the parent directory to 0700, and checks `SO_PEERCRED` before
reading a byte), so reading it already requires being that user — and that user
can read the daemon's memory, where the secret must live anyway for the
provider to use it. Encrypting the hop would protect nothing that is not
already lost, and would add a key-exchange to the one protocol surface that has
none.

What is NOT accepted is the secret leaking on the way.
`ConnectionProvideSecretParams` has a hand-written `Debug` that prints `***`
(pinned by a doctest, because the leak would return the day someone adds
`Debug` to the derive to fix something else); the dispatch span is `skip_all`;
both `Engine::provide_secret` and the connector instrument with `skip_all` and
log the connection name alone; and the TUI's typed buffer is a `TypedSecret`
whose `Debug` redacts. The modal paints one dot per character.

**Which copies are actually wiped, and which are not** — the honest version,
because "it is zeroized" is easy to write and only partly true. Wiped: the
TUI's buffer, whose `Zeroizing` interior is erased over its full capacity on
drop, and which is born with its maximum capacity reserved so that growing it
never abandons an un-wiped fragment on the heap (zeroize's own documentation is
explicit that it "cannot ensure that previous reallocations did not leave
values on the heap"). Not wiped: everything past `expose()` — the plain
`String` in the params, the serialised frame, the `serde_json::Value` the
daemon parses, and the copy handed to `Secret::new`. Under the same-user threat
model above those are acceptable; a core dump of the daemon is the residual
exposure, and it is residual only because anyone who can take one could read
the live secret anyway.

The dot count is **not** a length defence: below the cap it is the exact
length, deliberately, because seeing a dot appear is the only confirmation a
keystroke landed in a field that shows nothing. The cap exists so a long
passphrase does not overflow the box.

**Downgrading is the one direction that hurts, and it is not on the wire.**
`ConnectionSpec` is `deny_unknown_fields`, so a pre-0.63 binary reading a
`connections.toml` that already carries `secret = "prompt"` does not fail that
one connection — it fails the whole file, and every connection in it. Only
reachable by downgrading or by a mixed install; removing the key fixes it.

**Not covered.** The window does not paint this dialog yet (#327 tracks it);
until then a GUI user on a `prompt` connection reads the `err-secret-needed`
message, which names the environment variable. And only navigation intercepts
`SecretNeeded`: reaching a `prompt` connection as the destination of a copy,
or through compare or sync, still surfaces that message instead of the dialog.
