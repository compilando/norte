//! Opt-in check for a newer norte release (ADR 0183).
//!
//! Nothing here runs unless a frontend asks, and a frontend asks only when
//! `[updates] check = true` or the reader typed `norte update check`. It
//! never installs anything: it answers "this version exists", and the
//! reader decides.
//!
//! At most one request a day ([`CHECK_EVERY_MS`]): the answer is cached in
//! the state directory ([`CACHE_FILE`]), and every start in between reads
//! the cache.
#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::cmp::Ordering;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The release list asked for. Prereleases come in it too: norte is in
/// alpha, and [`Cache::newer_than`] decides which channel a reader is
/// offered.
pub const RELEASES_URL: &str = "https://api.github.com/repos/compilando/norte/releases?per_page=30";

/// Where a release's notes and downloads are: what a notice points at.
///
/// ```
/// assert!(norte_update::release_url("0.4.0").ends_with("/releases/tag/v0.4.0"));
/// ```
#[must_use]
pub fn release_url(version: &str) -> String {
    format!("https://github.com/compilando/norte/releases/tag/v{version}")
}

/// Now, in the UTC milliseconds the cache counts in. A clock before 1970
/// reads as 0, which makes any cache due: asking is the safe side.
#[must_use]
pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// One day: no reader needs to hear about a release sooner.
pub const CHECK_EVERY_MS: i64 = 24 * 60 * 60 * 1000;

/// Name of the cache inside the state directory.
pub const CACHE_FILE: &str = "update-check.json";

/// The response is a list of thirty releases. Anything far bigger is not
/// GitHub's answer, and is not read.
const MAX_BODY_BYTES: usize = 2 * 1024 * 1024;

/// Why a check could not answer.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The request failed: no network, a timeout, a non-2xx status.
    #[error("could not reach the release list: {0}")]
    Http(#[from] reqwest::Error),
    /// The answer was not the expected list.
    #[error("the release list did not parse: {0}")]
    Parse(#[from] serde_json::Error),
    /// The answer exceeded the size cap.
    #[error("the release list is larger than {MAX_BODY_BYTES} bytes")]
    TooLarge,
}

/// One entry of the release list: only what the decision reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    /// The tag, `v0.3.0-alpha.12`.
    pub tag_name: String,
    /// GitHub's prerelease flag.
    #[serde(default)]
    pub prerelease: bool,
    /// A draft is not published; it is never offered.
    #[serde(default)]
    pub draft: bool,
}

/// What the last request answered, kept between starts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cache {
    /// When it was asked, UTC milliseconds.
    pub checked_at_ms: i64,
    /// Highest stable release then, without the `v`.
    pub latest_stable: Option<String>,
    /// Highest release then, prereleases included, without the `v`.
    pub latest_any: Option<String>,
}

impl Cache {
    /// The cache a release list produces.
    ///
    /// ```
    /// use norte_update::{Cache, Release};
    /// let r = |t: &str, pre| Release { tag_name: t.into(), prerelease: pre, draft: false };
    /// let c = Cache::from_releases(&[r("v0.3.0", false), r("v0.4.0-alpha.1", true)], 7);
    /// assert_eq!(c.latest_stable.as_deref(), Some("0.3.0"));
    /// assert_eq!(c.latest_any.as_deref(), Some("0.4.0-alpha.1"));
    /// ```
    #[must_use]
    pub fn from_releases(releases: &[Release], now_ms: i64) -> Self {
        let highest = |stable_only: bool| {
            releases
                .iter()
                .filter(|r| !r.draft)
                .filter(|r| !(stable_only && r.prerelease))
                .filter_map(|r| Version::parse(r.tag_name.strip_prefix('v').unwrap_or(&r.tag_name)))
                .max()
                // RE-RENDERED, never the tag itself: whatever rode along in
                // it (build metadata, bidi characters) does not reach a
                // screen.
                .map(|v| v.to_string())
        };
        Self {
            checked_at_ms: now_ms,
            latest_stable: highest(true),
            latest_any: highest(false),
        }
    }

    /// Whether this cache is old enough to ask again.
    ///
    /// ```
    /// use norte_update::{Cache, CHECK_EVERY_MS};
    /// let c = Cache { checked_at_ms: 0, latest_stable: None, latest_any: None };
    /// assert!(!c.is_due(CHECK_EVERY_MS - 1));
    /// assert!(c.is_due(CHECK_EVERY_MS));
    /// ```
    #[must_use]
    pub fn is_due(&self, now_ms: i64) -> bool {
        // A clock moved backwards also asks: a cache "from the future"
        // would otherwise stop the checks until that date.
        now_ms < self.checked_at_ms || now_ms - self.checked_at_ms >= CHECK_EVERY_MS
    }

    /// The release to offer a reader running `current`, if any is newer.
    /// A reader on a prerelease is offered prereleases; a reader on a
    /// stable release only stable ones.
    ///
    /// ```
    /// use norte_update::Cache;
    /// let c = Cache {
    ///     checked_at_ms: 0,
    ///     latest_stable: Some("0.3.0".into()),
    ///     latest_any: Some("0.4.0-alpha.1".into()),
    /// };
    /// assert_eq!(c.newer_than("0.3.0-alpha.12").as_deref(), Some("0.4.0-alpha.1"));
    /// assert_eq!(c.newer_than("0.2.0").as_deref(), Some("0.3.0"));
    /// assert_eq!(c.newer_than("0.3.0"), None);
    /// ```
    #[must_use]
    pub fn newer_than(&self, current: &str) -> Option<String> {
        let current_v = Version::parse(current)?;
        let candidate = if current_v.pre.is_empty() {
            self.latest_stable.as_ref()
        } else {
            self.latest_any.as_ref()
        }?;
        // The cache is a file anyone on this account can edit: what it
        // holds is parsed again and RE-RENDERED, like a fresh answer.
        let candidate = Version::parse(candidate)?;
        (candidate > current_v).then(|| candidate.to_string())
    }
}

/// Reads the cache from `state_dir`. Missing or unreadable is `None`: the
/// next check asks again, which is the right repair.
///
/// Synchronous file I/O, like the rest of the state directory
/// (`norte_config::command_history`): call it off the async executor.
#[must_use]
pub fn read_cache(state_dir: &Path) -> Option<Cache> {
    let raw = std::fs::read(state_dir.join(CACHE_FILE)).ok()?;
    serde_json::from_slice(&raw).ok()
}

/// Writes the cache into `state_dir`: temporary file, then rename, so a
/// crash leaves the old cache or the new one.
///
/// # Errors
/// The directory cannot be created or written.
pub fn write_cache(state_dir: &Path, cache: &Cache) -> std::io::Result<()> {
    std::fs::create_dir_all(state_dir)?;
    let tmp = state_dir.join(format!("{CACHE_FILE}.tmp-{}", std::process::id()));
    std::fs::write(
        &tmp,
        serde_json::to_vec(cache).map_err(std::io::Error::other)?,
    )?;
    std::fs::rename(&tmp, state_dir.join(CACHE_FILE))
}

/// What a frontend calls at start when `[updates] check` is on: the
/// release to offer, if any. Asks the network only when the cache in
/// `state_dir` is [due](Cache::is_due); otherwise answers from the cache.
///
/// A failed request answers from the old cache if there is one, so a day
/// without network is not a day without the notice. Either way the cache
/// is stamped with `now_ms`: a machine offline, or a GitHub that says 403,
/// asks again tomorrow, not on every start. A cache that cannot be written
/// does not lose the answer in hand; it only means the next start asks.
///
/// # Errors
/// The request failed and there was no cache to fall back on.
pub async fn check_if_due(
    state_dir: std::path::PathBuf,
    current: &str,
    now_ms: i64,
) -> Result<Option<String>, UpdateError> {
    let dir = state_dir.clone();
    let cached = tokio::task::spawn_blocking(move || read_cache(&dir))
        .await
        .ok()
        .flatten();
    if let Some(cache) = cached.as_ref().filter(|c| !c.is_due(now_ms)) {
        return Ok(cache.newer_than(current));
    }
    let (stamped, result) = match fetch_releases(current).await {
        Ok(releases) => {
            let fresh = Cache::from_releases(&releases, now_ms);
            let answer = fresh.newer_than(current);
            (fresh, Ok(answer))
        }
        Err(e) => {
            let old = cached.unwrap_or(Cache {
                checked_at_ms: now_ms,
                latest_stable: None,
                latest_any: None,
            });
            let answer = old.newer_than(current);
            let result = if old.latest_any.is_some() {
                Ok(answer)
            } else {
                Err(e)
            };
            (
                Cache {
                    checked_at_ms: now_ms,
                    ..old
                },
                result,
            )
        }
    };
    let _ = tokio::task::spawn_blocking(move || write_cache(&state_dir, &stamped)).await;
    result
}

/// Asks [`RELEASES_URL`] for the release list.
///
/// # Errors
/// [`UpdateError::Http`], [`UpdateError::Parse`] or
/// [`UpdateError::TooLarge`].
pub async fn fetch_releases(current: &str) -> Result<Vec<Release>, UpdateError> {
    let client = reqwest::Client::builder()
        // GitHub refuses a request without a User-Agent.
        .user_agent(format!("norte/{current}"))
        .timeout(std::time::Duration::from_secs(10))
        .build()?;
    let mut response = client
        .get(RELEASES_URL)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await?
        .error_for_status()?;
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body.len() + chunk.len() > MAX_BODY_BYTES {
            return Err(UpdateError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(serde_json::from_slice(&body)?)
}

/// A semver version, ordered by semver precedence (build metadata
/// ignored). Only what [`Cache`] needs; not public API.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Version {
    core: (u64, u64, u64),
    pre: Vec<Ident>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Ident {
    Num(u64),
    Alpha(String),
}

/// Longer than any version norte will tag; past it, not a version.
const MAX_VERSION_BYTES: usize = 64;

impl Version {
    fn parse(v: &str) -> Option<Self> {
        if v.len() > MAX_VERSION_BYTES {
            return None;
        }
        // Build metadata is checked, then dropped: it does not order, and
        // the rendering never carries it.
        let v = match v.split_once('+') {
            Some((head, build)) => {
                let ok = build.split('.').all(|id| {
                    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                });
                if !ok {
                    return None;
                }
                head
            }
            None => v,
        };
        let (core, pre) = match v.split_once('-') {
            Some((core, pre)) => (core, Some(pre)),
            None => (v, None),
        };
        let num = |s: &str| {
            let ok = !s.is_empty()
                && s.bytes().all(|b| b.is_ascii_digit())
                && !(s.len() > 1 && s.starts_with('0'));
            ok.then(|| s.parse::<u64>().ok()).flatten()
        };
        let mut parts = core.split('.');
        let core = (
            num(parts.next()?)?,
            num(parts.next()?)?,
            num(parts.next()?)?,
        );
        if parts.next().is_some() {
            return None;
        }
        let pre = match pre {
            None => Vec::new(),
            Some(pre) => pre
                .split('.')
                .map(|id| {
                    if id.is_empty() || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    {
                        None
                    } else if let Some(n) = num(id) {
                        Some(Ident::Num(n))
                    } else if id.bytes().all(|b| b.is_ascii_digit()) {
                        None // a numeric identifier with a leading zero
                    } else {
                        Some(Ident::Alpha(id.to_owned()))
                    }
                })
                .collect::<Option<Vec<_>>>()?,
        };
        Some(Self { core, pre })
    }
}

impl Ord for Ident {
    fn cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Num(a), Self::Num(b)) => a.cmp(b),
            (Self::Alpha(a), Self::Alpha(b)) => a.cmp(b),
            (Self::Num(_), Self::Alpha(_)) => Ordering::Less,
            (Self::Alpha(_), Self::Num(_)) => Ordering::Greater,
        }
    }
}

impl PartialOrd for Ident {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        self.core.cmp(&other.core).then_with(|| {
            match (self.pre.is_empty(), other.pre.is_empty()) {
                (true, true) => Ordering::Equal,
                // A release ranks above its own prereleases.
                (true, false) => Ordering::Greater,
                (false, true) => Ordering::Less,
                (false, false) => self.pre.cmp(&other.pre),
            }
        })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// The canonical rendering: digits, dots, hyphens and the prerelease's
/// own `[0-9A-Za-z-]`. This, not the input, is what a screen gets.
impl std::fmt::Display for Version {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (major, minor, patch) = self.core;
        write!(f, "{major}.{minor}.{patch}")?;
        for (i, id) in self.pre.iter().enumerate() {
            f.write_str(if i == 0 { "-" } else { "." })?;
            match id {
                Ident::Num(n) => write!(f, "{n}")?,
                Ident::Alpha(s) => f.write_str(s)?,
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap_or_else(|| panic!("{s} parses"))
    }

    /// The precedence list from semver.org §11, in order.
    #[test]
    fn versions_order_as_semver_says() {
        let chain = [
            "1.0.0-alpha",
            "1.0.0-alpha.1",
            "1.0.0-alpha.beta",
            "1.0.0-beta",
            "1.0.0-beta.2",
            "1.0.0-beta.11",
            "1.0.0-rc.1",
            "1.0.0",
        ];
        for pair in chain.windows(2) {
            assert!(v(pair[0]) < v(pair[1]), "{} < {}", pair[0], pair[1]);
        }
        // Numeric, not lexical: alpha.12 is after alpha.9.
        assert!(v("0.3.0-alpha.12") > v("0.3.0-alpha.9"));
        assert_eq!(v("1.0.0+build"), v("1.0.0"));
    }

    #[test]
    fn what_is_not_a_version_is_not_offered() {
        for bad in [
            "",
            "1.0",
            "1.0.0.0",
            "01.0.0",
            "1.0.0-",
            "1.0.0-alpha..1",
            "1.0.0-01",
            "latest",
            "1.0.+0",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad:?}");
        }
        let c = Cache::from_releases(
            &[Release {
                tag_name: "nightly".into(),
                prerelease: true,
                draft: false,
            }],
            0,
        );
        assert_eq!(c.latest_any, None);
    }

    /// What reaches a screen is the parsed version re-rendered, never the
    /// tag: an escape or a bidi override in the build metadata either fails
    /// to parse or does not survive the rendering.
    #[test]
    fn nothing_but_a_rendered_version_reaches_a_screen() {
        for hostile in ["1.0.0+\u{1b}[2J", "1.0.0+", "1.0.0+a..b", "9.0.0+\u{202e}x"] {
            assert!(Version::parse(hostile).is_none(), "{hostile:?}");
        }
        assert!(Version::parse(&format!("1.0.0-{}", "a".repeat(80))).is_none());
        let c = Cache::from_releases(
            &[Release {
                tag_name: "v1.2.3-rc.1+build.7".into(),
                prerelease: true,
                draft: false,
            }],
            0,
        );
        assert_eq!(
            c.latest_any.as_deref(),
            Some("1.2.3-rc.1"),
            "the build tag is not kept"
        );
        // A cache edited by hand is read the same way.
        let edited = Cache {
            checked_at_ms: 0,
            latest_stable: Some("9.0.0+\u{202e}x".into()),
            latest_any: None,
        };
        assert_eq!(edited.newer_than("1.0.0"), None);
    }

    #[test]
    fn drafts_are_never_offered() {
        let c = Cache::from_releases(
            &[
                Release {
                    tag_name: "v0.3.0".into(),
                    prerelease: false,
                    draft: false,
                },
                Release {
                    tag_name: "v9.0.0".into(),
                    prerelease: false,
                    draft: true,
                },
            ],
            0,
        );
        assert_eq!(c.latest_stable.as_deref(), Some("0.3.0"));
    }

    #[test]
    fn a_day_old_cache_is_due_and_a_fresh_one_is_not() {
        let c = Cache {
            checked_at_ms: 1_000,
            latest_stable: None,
            latest_any: None,
        };
        assert!(!c.is_due(1_000 + CHECK_EVERY_MS - 1));
        assert!(c.is_due(1_000 + CHECK_EVERY_MS));
        assert!(c.is_due(999), "a clock moved backwards asks again");
    }

    #[test]
    fn the_cache_round_trips_and_a_broken_one_reads_as_none() {
        let dir = tempfile::tempdir().expect("tmp");
        assert_eq!(read_cache(dir.path()), None);
        let c = Cache {
            checked_at_ms: 5,
            latest_stable: Some("0.3.0".into()),
            latest_any: Some("0.4.0-alpha.1".into()),
        };
        write_cache(&dir.path().join("state"), &c).expect("write");
        assert_eq!(read_cache(&dir.path().join("state")), Some(c));
        std::fs::write(dir.path().join("state").join(CACHE_FILE), b"{").expect("write");
        assert_eq!(read_cache(&dir.path().join("state")), None);
    }

    /// A fresh cache answers without the network: this test has none, and
    /// passes because nothing is asked.
    #[tokio::test]
    async fn a_fresh_cache_answers_without_asking() {
        let dir = tempfile::tempdir().expect("tmp");
        let cache = Cache {
            checked_at_ms: 1_000,
            latest_stable: Some("0.4.0".into()),
            latest_any: Some("0.4.0".into()),
        };
        write_cache(dir.path(), &cache).expect("write");
        let answer = check_if_due(dir.path().to_path_buf(), "0.3.0", 2_000)
            .await
            .expect("answered from the cache");
        assert_eq!(answer.as_deref(), Some("0.4.0"));
    }

    /// GitHub's real shape, trimmed: unknown fields are ignored.
    #[test]
    fn the_github_answer_parses() {
        let raw = r#"[{"tag_name":"v0.3.0-alpha.12","prerelease":true,"draft":false,"name":"x","assets":[]}]"#;
        let list: Vec<Release> = serde_json::from_str(raw).expect("parses");
        assert_eq!(list[0].tag_name, "v0.3.0-alpha.12");
        assert!(list[0].prerelease);
    }
}
