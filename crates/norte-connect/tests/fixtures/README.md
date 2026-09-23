# norte-connect test fixtures

## `id_rsa_test`

This private RSA key was generated exclusively for tests. It has never been
used by a real system and has no value as a secret.

The fixture verifies that the connector rejects RSA keys with
`ConnectError::KeyUnsupported` (ADR 0015 E, issue #36, and
RUSTSEC-2023-0071), and that the same key authenticates with rsa-sha2 once a
connection opts in with `allow_rsa` (ADR 0150). Generating an RSA key during each debug test run would add
tens of seconds to the suite.

Secret scanners such as Gitleaks or GitHub secret scanning may report this file.
That is an expected false positive; add this fixture to the scanner's allowlist.
