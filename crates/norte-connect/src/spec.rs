//! `connections.toml`: ONLY references (rule 10), never secrets (ADR 0015 B).
//!
//! ```toml
//! [connections.work]
//! url = "sftp://oscar@sftp.example.com:22"
//! auth = "key"
//! key = "~/.ssh/id_ed25519"
//!
//! [connections.backup]
//! url = "ftp://backup@ftp.example.com:21"
//! auth = "password"
//! tls = "require"
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::error::ConnectError;

/// Parsed `connections.toml` file.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionsFile {
    /// Connections by name.
    #[serde(default)]
    pub connections: BTreeMap<String, ConnectionSpec>,
}

/// A remote connection: a reference, never the secret (ADR 0015).
///
/// `deny_unknown_fields`: an unexpected field (e.g. an inline `password = "…"`
/// the user tries to put here) is a LOUD ERROR, not silently ignored —
/// secrets go to the keyring/env/age, never to plain config.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConnectionSpec {
    /// `scheme://[user@]host[:port]` (for s3: `s3://bucket`, no user/port).
    pub url: String,
    /// Auth method. Default: `agent` (SSH agent / anonymous / opendal's
    /// ambient chain on s3).
    #[serde(default)]
    pub auth: AuthMethod,
    /// Path to the private key (for `auth = "key"`). NEVER the secret
    /// itself: the key's passphrase is resolved by the `SecretResolver`.
    pub key: Option<PathBuf>,
    /// TLS policy for FTP. Default: `require` (FTPS).
    #[serde(default)]
    pub tls: TlsMode,
    /// (s3) Bucket region. Mandatory with an AWS endpoint; with a custom
    /// endpoint (`MinIO`) `us-east-1` is assumed if missing.
    #[serde(default)]
    pub region: Option<String>,
    /// (s3) Service endpoint (`https://minio.interno:9000`). Absent = AWS.
    /// http = visible opt-in (insecure).
    #[serde(default)]
    pub endpoint: Option<String>,
    /// (s3) Access key id — NOT a secret (public identifier): it can go in
    /// config. The secret-access-key DOES go through the `SecretResolver`.
    #[serde(default)]
    pub access_key_id: Option<String>,
    /// (s3) Addressing style. Default: virtual-host without an endpoint
    /// (AWS), path with a custom endpoint (`MinIO` convention).
    #[serde(default)]
    pub addressing: Option<AddressingStyle>,
    /// Logical `.norte-trash/` trash on this connection (ADR 0019). Off by
    /// default: delete degrades to permanent with a frontend warning.
    #[serde(default)]
    pub logical_trash: bool,
    /// (sftp, `auth = "key"`) Accepts an RSA client key (ADR 0150).
    ///
    /// Off by default: without it RSA is rejected as always (ADR 0015). With
    /// it, signing goes through the `rsa` crate, the RUSTSEC-2023-0071
    /// (Marvin) path that 0015 closed, so it is an ACCEPTED risk per
    /// connection and not a preference: every connection that signs with RSA
    /// warns about it in the log, and `norte doctor` keeps reminding while
    /// it is set. Only rsa-sha2: a server that only accepts `ssh-rsa`
    /// (SHA-1) is rejected all the same.
    #[serde(default)]
    pub allow_rsa: bool,
    /// Where the secret comes from when the usual three rungs do not have it
    /// (#325).
    ///
    /// A CLOSED value and not an expression, and this was decided on
    /// purpose: a grammar here would invite `${env:…}` and `$(command)` in a
    /// file that is read at startup, and rule 10 exists precisely so that
    /// does not happen there. If another source is ever needed, add another
    /// value, not a syntax.
    #[serde(default)]
    pub secret: SecretSource,
}

/// How to authenticate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthMethod {
    /// SSH agent (sftp), anonymous (ftp), or opendal's ambient chain (s3:
    /// `AWS_*`/profile/IMDS — the CI/corporate case).
    #[default]
    Agent,
    /// Private key (`key = …`), passphrase via the resolver.
    Key,
    /// Password via the resolver.
    Password,
    /// (s3) Access key: `access_key_id` in config + secret-access-key via
    /// the resolver.
    ///
    /// It is deterministic because explicit credentials WIN, not because the
    /// ambient chain is off: `disable_config_load` only turns off env,
    /// profile and IMDS, and in opendal 0.58 it leaves SSO, web-identity,
    /// process and ECS in place (#321). What keeps that chain from being
    /// reached is that the connector rejects an `access_key_id` or a secret
    /// that is missing or EMPTY (#320).
    AccessKey,
}

/// Where a connection's secret comes from when it is not where it is always
/// looked for (#325).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SecretSource {
    /// Only the usual three rungs: `NORTE_SECRET_<CONN>`, keyring,
    /// `secrets.age`. If none has it, the connection fails — which is what
    /// norte did until #325.
    #[default]
    Stored,
    /// And if none has it, ASK whoever is at the keyboard.
    ///
    /// The fourth rung and not the first: a CI machine with the variable set
    /// never sees a dialog, and a laptop does not need the variable. What
    /// gets typed lives in memory for as long as the daemon's session lasts
    /// and is not written anywhere.
    ///
    /// **Only with `auth = "password"` and `auth = "access-key"`.** With
    /// `agent` there is no secret to ask for, and with `key` the secret is
    /// the key's passphrase, where empty and absent are the same thing —
    /// asking there would pop up a dialog every time someone uses an
    /// unencrypted key. `norte doctor` warns (`conn-secret-prompt-inert`) if
    /// this is set somewhere it does nothing.
    Prompt,
}

/// S3 addressing style (ADR 0016 I).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AddressingStyle {
    /// `https://bucket.host/key` (AWS default).
    VirtualHost,
    /// `https://host/bucket/key` (`MinIO` and S3-compatibles).
    Path,
}

/// FTP TLS policy (ADR 0014/0015 F).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TlsMode {
    /// FTPS mandatory (AUTH TLS). Safe default.
    #[default]
    Require,
    /// Tries TLS; if the server REJECTS `AUTH TLS`, falls back to plain with
    /// a warning. WATCH OUT: does not protect against an ACTIVE attacker (it
    /// can suppress the AUTH and receive the credentials in the clear). A
    /// handshake/validation failure with AUTH already accepted does NOT
    /// degrade (fail-closed: possible MITM).
    Allow,
    /// Plain FTP (insecure): EXPLICIT opt-in.
    Plain,
}

/// Endpoint extracted from a `url` (`scheme://[user@]host[:port]`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    /// `sftp` | `ftp`.
    pub scheme: String,
    /// User, if the URL carries one.
    pub user: Option<String>,
    /// Host (without IPv6's `[]`).
    pub host: String,
    /// Port, if the URL carries one.
    pub port: Option<u16>,
}

impl Endpoint {
    /// `scheme://host[:port]` to SHOW, without userinfo (#325).
    ///
    /// The user is dropped on purpose: the same redaction `ConnectionDegraded`
    /// applies to its `host`, and for the same reason — a `user:pass@` in
    /// the URL must not reach the screen or the log (rule 10). What is left
    /// is what makes a password dialog answerable: WHO it is going to be
    /// given to.
    ///
    /// ```
    /// # use norte_connect::ConnectionSpec;
    /// let spec: ConnectionSpec =
    ///     toml::from_str("url = \"sftp://oscar@host.example:2222\"").unwrap();
    /// assert_eq!(spec.endpoint().unwrap().display(), "sftp://host.example:2222");
    /// ```
    #[must_use]
    pub fn display(&self) -> String {
        match self.port {
            Some(p) => format!("{}://{}:{p}", self.scheme, self.host),
            None => format!("{}://{}", self.scheme, self.host),
        }
    }
}

impl ConnectionSpec {
    /// Parses the `url`'s `scheme://[user@]host[:port]`.
    ///
    /// # Errors
    /// If the URL does not have the expected shape.
    pub fn endpoint(&self) -> Result<Endpoint, ConnectError> {
        parse_endpoint(&self.url)
    }

    /// Where this connection really goes, to SHOW when asking for the secret
    /// (#325). No userinfo, in either half.
    ///
    /// The URL alone is not enough. In `s3` the URL's "host" is the BUCKET,
    /// and the server that is going to receive the signed credential is the
    /// entry's `endpoint =` — which is exactly the piece a foreign
    /// `connections.toml` can point somewhere else. Showing only
    /// `s3://mi-bucket` would tell the half that does not matter. When there
    /// is an explicit endpoint, both are shown, separated by `@`.
    ///
    /// ```
    /// # use norte_connect::ConnectionSpec;
    /// let s: ConnectionSpec = toml::from_str(
    ///     "url = \"s3://mi.bucket\"\nendpoint = \"https://oscar@s3.eu-west-1.example\"",
    /// )
    /// .unwrap();
    /// assert_eq!(
    ///     s.destination_display().unwrap(),
    ///     "s3://mi.bucket @ https://s3.eu-west-1.example"
    /// );
    /// ```
    ///
    /// # Errors
    /// If the URL does not have the expected shape.
    pub fn destination_display(&self) -> Result<String, ConnectError> {
        let base = self.endpoint()?.display();
        match self.endpoint.as_deref() {
            Some(ep) if !ep.is_empty() => Ok(format!("{base} @ {}", sin_userinfo(ep))),
            _ => Ok(base),
        }
    }
}

/// Strips the `user[:pass]@` from a config URL, leaving the rest as is. An
/// `endpoint =` is written by a person and can carry credentials inside;
/// this goes to the screen and the log (rule 10).
fn sin_userinfo(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        // No scheme means no authority to trim: return it whole, which is
        // more honest than guessing where it starts.
        return url.to_string();
    };
    // The authority's `@` is the LAST one before the first `/`, because a
    // password can carry at-signs.
    let (authority, tail) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, ""),
    };
    let clean = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    format!("{scheme}://{clean}{tail}")
}

/// Minimal parser for `scheme://[user@]host[:port]` (no path). Avoids a
/// full URL dependency. The scheme is anything valid for a `VPath`: the core
/// decides afterward whether it serves it itself or a plugin provider that
/// declares it.
fn parse_endpoint(url: &str) -> Result<Endpoint, ConnectError> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| ConnectError::InvalidUrl(url.to_string()))?;
    // Authority only: discards any accidental `/path`.
    let authority = rest.split('/').next().unwrap_or(rest);
    // LAST `@` (not the first): a pathological authority `u@a:b@h` must not
    // let a `:` in a middle segment sneak through and then fail on an
    // invalid port while echoing the URL with the secret. Aligns with
    // proto's Authority::new (#46).
    let (user, hostport) = match authority.rsplit_once('@') {
        // `@` without a user (`sftp://@host`) is a malformed URL, not a host.
        Some(("", _)) => return Err(ConnectError::InvalidUrl(url.to_string())),
        // `user:pass@host` is NOT accepted (rule 10: the secret would go to
        // config/logs). STATIC message, and this check goes BEFORE the
        // scheme's: no later error can echo a URL with a password (e.g. the
        // typo `ftps://u:pass@h` would otherwise die on scheme while echoing
        // the secret).
        Some((u, _)) if u.contains(':') => {
            return Err(ConnectError::InvalidUrl(
                "the URL must not carry an inline password (user:pass@…); the secret goes \
                 through the keyring/env/secrets.age"
                    .to_string(),
            ));
        }
        Some((u, hp)) => (Some(u.to_string()), hp),
        None => (None, authority),
    };
    // Any scheme a `VPath` can carry: the core's and whatever a plugin
    // provider declares. The closed `sftp|ftp|s3` list that used to be here
    // made it impossible for a plugin to serve `webdav://` without touching
    // this crate, which is exactly what a plugin must not touch.
    if norte_proto::Scheme::new(scheme).is_err() {
        return Err(ConnectError::InvalidUrl(url.to_string()));
    }
    // s3://bucket: the authority is ONLY the bucket. A `user@` in the user
    // position would smell like a credential in the URL (rule 10) and the
    // port goes in the `endpoint` field, not the authority — both are
    // rejected.
    if scheme == "s3" && user.is_some() {
        return Err(ConnectError::InvalidUrl(
            "s3://bucket does not carry user@ (credentials go through access_key_id + the \
             resolver)"
                .to_string(),
        ));
    }
    // IPv6 ALWAYS between `[...]`; outside brackets a leftover `:` in the
    // host would be an unbracketed IPv6 (ambiguous) → invalid.
    let (host, port) = if let Some(rest) = hostport.strip_prefix('[') {
        let (h, tail) = rest
            .split_once(']')
            .ok_or_else(|| ConnectError::InvalidUrl(url.to_string()))?;
        (h.to_string(), parse_port(tail.strip_prefix(':'), url)?)
    } else if let Some((h, p)) = hostport.rsplit_once(':') {
        if h.contains(':') {
            return Err(ConnectError::InvalidUrl(url.to_string()));
        }
        (h.to_string(), parse_port(Some(p), url)?)
    } else if hostport.contains(':') {
        return Err(ConnectError::InvalidUrl(url.to_string()));
    } else {
        (hostport.to_string(), None)
    };
    if host.is_empty() || !is_valid_host(&host) {
        return Err(ConnectError::InvalidUrl(url.to_string()));
    }
    if scheme == "s3" {
        if port.is_some() {
            return Err(ConnectError::InvalidUrl(
                "s3://bucket does not carry a port in the authority; use the `endpoint` field"
                    .to_string(),
            ));
        }
        // The bucket is injected RAW into the URL (virtual-host:
        // `//{bucket}.host`) without percent-encoding: it is validated with
        // AWS's naming rules, not with `is_valid_host`'s lax charset (meant
        // for known_hosts).
        if !is_valid_bucket(&host) {
            return Err(ConnectError::InvalidUrl(
                "invalid s3 bucket name (3-63, lowercase alphanumeric + `-`/`.`, no `..`)"
                    .to_string(),
            ));
        }
    }
    Ok(Endpoint {
        scheme: scheme.to_string(),
        user,
        host,
        port,
    })
}

/// Hostname/IP charset (incl. IPv6 with a zone: `:`/`%`). Excludes EVERYTHING
/// that has meaning in the `known_hosts` format (`,` host list, space and
/// newline separators, `#` comment, `|` hash) and control characters: a
/// hostile host cannot poison other entries via `learn`.
fn is_valid_host(host: &str) -> bool {
    host.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | ':' | '%'))
}

/// S3 bucket naming rules (safe subset): 3-63 bytes, lowercase alphanumeric +
/// `-`/`.`, starts and ends alphanumeric, no `..` (which would break the
/// virtual-host `//{bucket}.host`). Does not cover the IP-format prohibition
/// (irrelevant for injection); AWS/opendal would reject it anyway.
fn is_valid_bucket(b: &str) -> bool {
    (3..=63).contains(&b.len())
        && b.bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        && b.bytes().next().is_some_and(|c| c.is_ascii_alphanumeric())
        && b.bytes().last().is_some_and(|c| c.is_ascii_alphanumeric())
        && !b.contains("..")
}

fn parse_port(p: Option<&str>, url: &str) -> Result<Option<u16>, ConnectError> {
    match p {
        None | Some("") => Ok(None),
        // Port 0 is not a valid destination port.
        Some(p) => match p.parse::<u16>() {
            Ok(0) | Err(_) => Err(ConnectError::InvalidUrl(url.to_string())),
            Ok(n) => Ok(Some(n)),
        },
    }
}

impl ConnectionsFile {
    /// Loads `<dir>/connections.toml`. If it does not exist, returns empty
    /// (not an error: connections are optional).
    ///
    /// SYNCHRONOUS on purpose: this is BOOTSTRAP config loading (once at
    /// startup, before entering the runtime, or from `spawn_blocking` if
    /// called in an async context). Not a hot path; does no network I/O.
    ///
    /// # Errors
    /// If the file exists but is invalid or unreadable TOML.
    pub fn load(dir: &Path) -> Result<Self, ConnectError> {
        let path = dir.join("connections.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => return Err(ConnectError::Io(e)),
        };
        toml::from_str(&text).map_err(|e| ConnectError::Config(e.to_string()))
    }

    /// Same, but **one unusable entry does not take the rest down with it**
    /// (#365).
    ///
    /// Returns the usable ones and, separately, the name and reason of the
    /// ones that are not.
    ///
    /// The difference with [`Self::load`] matters because the two questions
    /// are different and have different answers. Whoever is about to
    /// CONNECT needs the whole entry or nothing, and there a failure is a
    /// failure. Whoever is about to LIST them —a selector— loses its entire
    /// list over a single entry norte cannot read, with an error that names
    /// no connection and points at nothing the reader can fix. That is worse
    /// than useless: it hides the nineteen that were fine.
    ///
    /// A file with a SYNTAX error is still one whole error, and it has to
    /// be: without being able to split it into entries there is nothing to
    /// salvage, and saying "you have none" over one extra comma would be a
    /// lie.
    ///
    /// # Errors
    /// If the file is unreadable, or its TOML does not parse even as a
    /// table.
    pub fn load_tolerante(dir: &Path) -> Result<(Self, Vec<(String, String)>), ConnectError> {
        let path = dir.join("connections.toml");
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok((Self::default(), Vec::new()));
            }
            Err(e) => return Err(ConnectError::Io(e)),
        };
        let raw: toml::Table =
            toml::from_str(&text).map_err(|e| ConnectError::Config(e.to_string()))?;
        let Some(table) = raw.get("connections").and_then(toml::Value::as_table) else {
            // No `connections` section means nothing to do, and it is not an
            // error: a file that only carries other sections is a valid
            // file.
            return Ok((Self::default(), Vec::new()));
        };
        let mut good = std::collections::BTreeMap::new();
        let mut bad = Vec::new();
        for (name, value) in table {
            match value.clone().try_into::<ConnectionSpec>() {
                Ok(spec) => {
                    good.insert(name.clone(), spec);
                }
                // The reason is stored as TEXT and goes to the interface: it
                // is the only thing that turns "one of your connections is
                // no good" into something actionable. It carries no
                // secrets — what fails is the shape of the entry, and
                // `ConnectionSpec` references its credentials instead of
                // storing them (ADR 0015).
                Err(e) => bad.push((name.clone(), e.to_string())),
            }
        }
        Ok((Self { connections: good }, bad))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logical_trash_defaults_off_and_parses() {
        // Absent → false (safe default, ADR 0019).
        let f: ConnectionsFile =
            toml::from_str("[connections.a]\nurl = \"sftp://h\"\n").expect("parse");
        assert!(!f.connections["a"].logical_trash);

        // Present → true.
        let f: ConnectionsFile =
            toml::from_str("[connections.b]\nurl = \"sftp://h\"\nlogical_trash = true\n")
                .expect("parse");
        assert!(f.connections["b"].logical_trash);
    }

    #[test]
    fn allow_rsa_defaults_off_and_parses() {
        // Absent → false: RSA is still rejected by default (ADR 0150).
        let f: ConnectionsFile =
            toml::from_str("[connections.a]\nurl = \"sftp://h\"\n").expect("parse");
        assert!(!f.connections["a"].allow_rsa);

        let f: ConnectionsFile =
            toml::from_str("[connections.b]\nurl = \"sftp://h\"\nallow_rsa = true\n")
                .expect("parse");
        assert!(f.connections["b"].allow_rsa);
    }

    #[test]
    fn parse_connections_toml() {
        let toml = r#"
            [connections.trabajo]
            url = "sftp://oscar@sftp.example.com:22"
            auth = "key"
            key = "/home/oscar/.ssh/id_ed25519"

            [connections.backup]
            url = "ftp://backup@ftp.example.com"
            auth = "password"
            tls = "plain"
        "#;
        let f: ConnectionsFile = toml::from_str(toml).unwrap();
        let t = &f.connections["trabajo"];
        assert_eq!(t.auth, AuthMethod::Key);
        assert_eq!(t.tls, TlsMode::Require); // default
        assert!(t.key.is_some());
        let ep = t.endpoint().unwrap();
        assert_eq!(ep.scheme, "sftp");
        assert_eq!(ep.user.as_deref(), Some("oscar"));
        assert_eq!(ep.host, "sftp.example.com");
        assert_eq!(ep.port, Some(22));
        assert_eq!(f.connections["backup"].tls, TlsMode::Plain);
    }

    #[test]
    fn endpoint_variantes() {
        let ep = parse_endpoint("sftp://host").unwrap();
        assert_eq!(ep.host, "host");
        assert_eq!(ep.user, None);
        assert_eq!(ep.port, None);
        let ep = parse_endpoint("ftp://u@[::1]:2121").unwrap();
        assert_eq!(ep.host, "::1");
        assert_eq!(ep.user.as_deref(), Some("u"));
        assert_eq!(ep.port, Some(2121));
    }

    /// A plugin provider serves the scheme it declares, so the connection
    /// parser cannot carry the closed `sftp|ftp|s3` list: `webdav://` gets
    /// here before anyone asks the catalogue. What is required is that it be
    /// a scheme (`norte_proto::Scheme`'s alphabet), not that it be one of
    /// the core's three.
    #[test]
    fn endpoint_accepts_a_plugin_scheme() {
        let ep = parse_endpoint("webdav://u@files.example.com:8443").unwrap();
        assert_eq!(ep.scheme, "webdav");
        assert_eq!(ep.user.as_deref(), Some("u"));
        assert_eq!(ep.host, "files.example.com");
        assert_eq!(ep.port, Some(8443));
        assert_eq!(parse_endpoint("memplug://host").unwrap().scheme, "memplug");
        // But what is not a scheme is still out: uppercase, empty, slashes.
        assert!(parse_endpoint("HTTP://host").is_err());
        assert!(parse_endpoint("://host").is_err());
        assert!(parse_endpoint("a/b://host").is_err());
    }

    #[test]
    fn endpoint_invalid() {
        assert!(parse_endpoint("sin-scheme").is_err());
        assert!(parse_endpoint("sftp://host:noport").is_err());
        assert!(parse_endpoint("sftp://").is_err()); // empty host
        assert!(parse_endpoint("sftp://@host").is_err()); // empty user
        assert!(parse_endpoint("sftp://host:0").is_err()); // port 0
        assert!(parse_endpoint("sftp://::1").is_err()); // IPv6 without brackets
    }

    /// An inline password in the URL (`user:pass@host`) is rejected WITHOUT
    /// echoing the URL: if it were accepted (or echoed in the error), the
    /// password would end up in connections.toml, in logs or in error
    /// messages (rule 10).
    #[test]
    fn inline_password_in_url_rejected_without_echo() {
        let err = parse_endpoint("sftp://u:hunter2@h").unwrap_err();
        assert!(
            !format!("{err}").contains("hunter2"),
            "the error must not echo the password"
        );
    }

    /// The `ftps://` typo (invalid scheme) with an inline password ALSO does
    /// not echo the URL: the userinfo check runs BEFORE the scheme's — if it
    /// did not, the scheme error would carry the password into the logs
    /// (rule 10).
    #[test]
    fn an_invalid_scheme_with_an_inline_password_is_not_echoed() {
        for url in ["ftps://u:hunter2@h", "http://u:hunter2@h"] {
            let err = parse_endpoint(url).unwrap_err();
            assert!(
                !format!("{err}").contains("hunter2"),
                "{url}: the error echoes the password"
            );
        }
    }

    /// The host does not accept characters that have meaning in
    /// `known_hosts` (`,` host list, space/newline separators, `#` comment,
    /// `|` hash) or control characters: if they slipped through, a `learn`
    /// could poison other entries.
    #[test]
    fn host_with_format_characters_is_rejected() {
        for url in [
            "sftp://banco.com,evil.com",
            "sftp://a b",
            "sftp://a\nb",
            "sftp://a#b",
            "sftp://a|b",
            "sftp://a\tb",
        ] {
            assert!(parse_endpoint(url).is_err(), "{url:?} should be invalid");
        }
    }

    #[test]
    fn deny_unknown_rejects_inline_secret() {
        // An inline `password` (rule 10) must be an ERROR, not ignored.
        let toml = r#"
            [connections.x]
            url = "sftp://h"
            password = "no-va-aqui"
        "#;
        assert!(toml::from_str::<ConnectionsFile>(toml).is_err());
    }

    #[test]
    fn parse_s3_connection() {
        let toml = r#"
            [connections.almacen]
            url = "s3://mi-bucket"
            auth = "access-key"
            access_key_id = "AKIAEXAMPLE"
            region = "eu-west-1"
            endpoint = "https://minio.interno:9000"
            addressing = "path"
        "#;
        let f: ConnectionsFile = toml::from_str(toml).unwrap();
        let s = &f.connections["almacen"];
        assert_eq!(s.auth, AuthMethod::AccessKey);
        assert_eq!(s.access_key_id.as_deref(), Some("AKIAEXAMPLE"));
        assert_eq!(s.region.as_deref(), Some("eu-west-1"));
        assert_eq!(s.endpoint.as_deref(), Some("https://minio.interno:9000"));
        assert_eq!(s.addressing, Some(AddressingStyle::Path));
        let ep = s.endpoint().unwrap();
        assert_eq!(ep.scheme, "s3");
        assert_eq!(ep.host, "mi-bucket"); // authority = bucket
        assert_eq!(ep.user, None);
        assert_eq!(ep.port, None);
    }

    /// The s3 fields are optional: an sftp/ftp connections.toml without them
    /// still parses with `deny_unknown_fields`.
    #[test]
    fn optional_s3_fields_do_not_break_sftp() {
        let s: ConnectionSpec = toml::from_str(r#"url = "sftp://h""#).unwrap();
        assert_eq!(s.region, None);
        assert_eq!(s.endpoint, None);
        assert_eq!(s.access_key_id, None);
        assert_eq!(s.addressing, None);
    }

    /// `s3://user@bucket` and `s3://bucket:9000` are rejected: s3's
    /// authority is ONLY the bucket (a user would smell like a credential,
    /// the port goes in `endpoint`).
    #[test]
    fn s3_with_user_or_port_is_rejected() {
        assert!(parse_endpoint("s3://user@bucket").is_err());
        assert!(parse_endpoint("s3://bucket:9000").is_err());
        // The bare bucket is fine.
        let ep = parse_endpoint("s3://mi-bucket").unwrap();
        assert_eq!(ep.host, "mi-bucket");
    }

    /// Invalid bucket names (AWS's charset, not `known_hosts`'s lax one):
    /// uppercase, `_`, `..`, non-alphanumeric ends, length outside 3-63.
    #[test]
    fn an_invalid_s3_bucket_is_rejected() {
        for bad in [
            "s3://MiBucket",   // uppercase
            "s3://mi_bucket",  // underscore
            "s3://mi..bucket", // double dot (breaks virtual-host)
            "s3://-bucket",    // starts non-alphanumeric
            "s3://bucket.",    // ends non-alphanumeric
            "s3://ab",         // <3
            "s3://a%evil",     // % (host's lax charset, not bucket's)
        ] {
            assert!(parse_endpoint(bad).is_err(), "{bad:?} should be invalid");
        }
        // Typical valid ones.
        assert!(parse_endpoint("s3://mi-bucket.prod").is_ok());
        assert!(parse_endpoint("s3://data123").is_ok());
    }

    /// An inline `secret_access_key` in connections.toml is an ERROR (rule
    /// 10): the secret goes through the resolver, never to plain config.
    #[test]
    fn secret_access_key_inline_rechazado() {
        let toml = r#"
            [connections.x]
            url = "s3://b"
            auth = "access-key"
            access_key_id = "AKIA"
            secret_access_key = "no-va-aqui"
        "#;
        assert!(toml::from_str::<ConnectionsFile>(toml).is_err());
    }

    #[test]
    fn load_ausente_es_empty() {
        let dir = tempfile::tempdir().unwrap();
        let f = ConnectionsFile::load(dir.path()).unwrap();
        assert!(f.connections.is_empty());
    }
}
