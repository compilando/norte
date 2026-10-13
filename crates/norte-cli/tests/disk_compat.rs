//! What a released norte left on disk still opens with this one.
//!
//! `tests/disk-compat/<version>/config/` is a `NORTE_CONFIG_DIR` written by
//! that release's own binary (or validated by it), frozen when the release
//! was cut; `expected/` is what that binary answered about it. Every corpus
//! in the directory runs, so a renamed key, a dropped journal column or a
//! changed hash input fails here instead of in a user's home. How to add one
//! is in `tests/disk-compat/README.md`.
//!
//! The binary works on a COPY: an upgrade is allowed to rewrite what it
//! opens, the corpus is not.
//!
//! Unix only: the corpora were written on Linux and their paths are
//! `file:///srv/...`. A Windows corpus would be its own directory.
#![cfg(unix)]

use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn corpora() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/disk-compat");
    let mut found: Vec<PathBuf> = std::fs::read_dir(&root)
        .expect("tests/disk-compat exists")
        .map(|e| e.expect("readable entry").path())
        .filter(|p| p.join("config").is_dir())
        .collect();
    found.sort();
    assert!(!found.is_empty(), "no corpus under {}", root.display());
    found
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).expect("mkdir");
    for entry in std::fs::read_dir(from).expect("read_dir") {
        let entry = entry.expect("entry");
        let dest = to.join(entry.file_name());
        if entry.file_type().expect("file type").is_dir() {
            copy_tree(&entry.path(), &dest);
        } else {
            std::fs::copy(entry.path(), &dest).expect("copy");
        }
    }
}

/// One corpus staged in a tempdir, with every directory norte resolves
/// pointed inside it.
struct Staged {
    _tmp: tempfile::TempDir,
    config: PathBuf,
    xdg_config: PathBuf,
    state: PathBuf,
    expected: PathBuf,
    name: String,
}

impl Staged {
    fn new(corpus: &Path) -> Self {
        let tmp = tempfile::TempDir::new_in(env!("CARGO_TARGET_TMPDIR")).expect("tempdir");
        let config = tmp.path().join("config");
        copy_tree(&corpus.join("config"), &config);
        // An empty XDG config home: with the developer's real
        // `~/.config/norte` in view, doctor warns about split-brain.
        let xdg_config = tmp.path().join("xdg");
        let state = tmp.path().join("state");
        std::fs::create_dir_all(&xdg_config).expect("mkdir");
        std::fs::create_dir_all(&state).expect("mkdir");
        Self {
            config,
            xdg_config,
            state,
            expected: corpus.join("expected"),
            name: corpus
                .file_name()
                .expect("named")
                .to_string_lossy()
                .into_owned(),
            _tmp: tmp,
        }
    }

    fn norte(&self) -> Command {
        let mut c = Command::cargo_bin("norte").expect("norte binary compiled");
        c.env("NORTE_CONFIG_DIR", &self.config)
            .env("XDG_CONFIG_HOME", &self.xdg_config)
            .env("XDG_STATE_HOME", &self.state)
            .env("NORTE_LANG", "en");
        c
    }

    fn expected(&self, file: &str) -> String {
        std::fs::read_to_string(self.expected.join(file)).expect("expected file")
    }

    fn journal(&self) -> Vec<serde_json::Value> {
        let out = self
            .norte()
            .args(["audit", "export", "--format", "jsonl"])
            .assert()
            .success();
        jsonl(&String::from_utf8_lossy(&out.get_output().stdout))
    }
}

fn jsonl(text: &str) -> Vec<serde_json::Value> {
    text.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).expect("a JSON line"))
        .collect()
}

#[test]
fn doctor_finds_no_error_and_no_new_warning_in_any_release() {
    for corpus in corpora() {
        let s = Staged::new(&corpus);
        let out = s.norte().args(["doctor", "--json"]).assert().success();
        let report: serde_json::Value =
            serde_json::from_slice(&out.get_output().stdout).expect("doctor --json");
        let allowed = s.expected("doctor-warnings.txt");
        let allowed: Vec<&str> = allowed.lines().map(str::trim).collect();
        for f in report["findings"].as_array().expect("findings") {
            let (severity, code) = (f["severity"].as_str(), f["code"].as_str().unwrap_or(""));
            assert!(
                severity == Some("ok") || (severity == Some("warn") && allowed.contains(&code)),
                "{}: {f}",
                s.name
            );
        }
    }
}

/// The export carries every entry's hash: equal values mean the rows were
/// read back, AND the chain recomputed to the same bytes.
#[test]
fn the_journal_reads_back_entry_for_entry() {
    for corpus in corpora() {
        let s = Staged::new(&corpus);
        assert_eq!(
            s.journal(),
            jsonl(&s.expected("journal.jsonl")),
            "{}",
            s.name
        );
    }
}

/// Reading is half an upgrade: the next mutation lands on the old file, and
/// the chain has to stay whole across the seam.
#[test]
fn an_old_journal_takes_new_entries_and_its_chain_stays_intact() {
    for corpus in corpora() {
        let s = Staged::new(&corpus);
        let before = jsonl(&s.expected("journal.jsonl"));
        let dir = s.state.join("after-upgrade");
        s.norte().arg("mkdir").arg(&dir).assert().success();
        let after = s.journal();
        assert_eq!(after.len(), before.len() + 1, "{}", s.name);
        assert_eq!(after[..before.len()], before[..], "{}", s.name);
        // Without anchors the verdict is a failure by design (ADR 0025);
        // what is pinned is the chain half of it.
        let out = s
            .norte()
            .args(["audit", "verify", "--allow-no-anchors"])
            .output()
            .expect("audit verify ran");
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        assert!(
            text.contains(&format!("hash chain intact ({} entries)", after.len())),
            "{}: {text}",
            s.name
        );
    }
}

#[test]
fn the_index_answers_the_same_query() {
    for corpus in corpora() {
        let s = Staged::new(&corpus);
        let out = s
            .norte()
            .args(["index", "query", "/srv/norte/w", "copia"])
            .assert()
            .success();
        assert_eq!(
            String::from_utf8_lossy(&out.get_output().stdout),
            s.expected("index-query-copia.txt"),
            "{}",
            s.name
        );
    }
}
