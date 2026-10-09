//! Shell profiles (`terminal.toml`): which shells the terminal panel's `+`
//! can start. "Shell profile", never bare "profile": norte already has
//! configuration profiles, and they are another thing.
//!
//! A shell profile names a program norte RUNS, so the project layer
//! (`./.norte`) never reaches here — same rule as `openers.toml`.
//!
//! ```toml
//! default = "fish"
//!
//! [[shell]]
//! name = "fish"
//! program = "/usr/bin/fish"
//! args = ["-l"]
//! icon = "terminal"
//! color = 4
//! ```

use std::ffi::OsString;
use std::path::PathBuf;

use serde::Deserialize;

use crate::terminals::{AnsiColor, TerminalIcon};

/// What is wrong with a `terminal.toml`.
#[derive(Debug, thiserror::Error)]
pub enum ShellProfileError {
    /// It does not parse, or has a key nobody reads.
    #[error("{0}")]
    Toml(String),
    /// A shell profile without a name.
    #[error("a [[shell]] has an empty `name`")]
    EmptyName,
    /// A relative `program` would be looked up from the directory being
    /// browsed (#302, ADR 0082).
    #[error("shell profile {name:?}: `program` must be an absolute path")]
    RelativeProgram {
        /// The shell profile.
        name: String,
    },
    /// The same name twice in ONE file: across layers a name overrides,
    /// inside one file it is a typo.
    #[error("shell profile {name:?} is defined twice in the same file")]
    DuplicateName {
        /// The repeated name.
        name: String,
    },
    /// `default` names no shell profile.
    #[error("`default = {name:?}` names no [[shell]]")]
    UnknownDefault {
        /// The name it gave.
        name: String,
    },
    /// `color` outside 1..=6.
    #[error("shell profile {name:?}: `color = {value}` must be 1 to 6")]
    BadColor {
        /// The shell profile.
        name: String,
        /// The value it gave.
        value: u8,
    },
    /// `icon` outside the fixed set.
    #[error("shell profile {name:?}: unknown `icon = {value:?}`")]
    BadIcon {
        /// The shell profile.
        name: String,
        /// The value it gave.
        value: String,
    },
}

/// One shell the panel can start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfile {
    /// Its name: the list entry and the instance's fallback title.
    pub name: String,
    /// The program, absolute.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
    /// The icon its instances start with.
    pub icon: Option<TerminalIcon>,
    /// The colour its instances start with.
    pub color: Option<AnsiColor>,
}

/// One layer's `terminal.toml`, validated but not merged.
#[derive(Debug, Clone)]
pub struct ShellProfilesFile {
    default: Option<String>,
    shells: Vec<ShellProfile>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    default: Option<String>,
    #[serde(default)]
    shell: Vec<RawShell>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawShell {
    name: String,
    program: String,
    #[serde(default)]
    args: Vec<String>,
    icon: Option<String>,
    color: Option<u8>,
}

impl ShellProfilesFile {
    /// Does this file set `default`? The one that does is the one to blame
    /// when it names nobody.
    #[must_use]
    pub fn sets_default(&self) -> bool {
        self.default.is_some()
    }
}

/// The merged shell profiles, with the one `+` starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellProfiles {
    shells: Vec<ShellProfile>,
    default: usize,
}

impl Default for ShellProfiles {
    fn default() -> Self {
        Self::implicit()
    }
}

impl ShellProfiles {
    /// No `terminal.toml` anywhere: the reader's login shell, named after
    /// its file name.
    #[must_use]
    pub fn implicit() -> Self {
        let program = crate::shell::login_shell();
        let name = program
            .file_name()
            .map_or_else(|| "shell".to_owned(), |n| n.to_string_lossy().into_owned());
        Self {
            shells: vec![ShellProfile {
                name,
                program,
                args: Vec::new(),
                icon: None,
                color: None,
            }],
            default: 0,
        }
    }

    /// Parses and validates one layer's file.
    ///
    /// # Errors
    /// Any [`ShellProfileError`] but [`ShellProfileError::UnknownDefault`],
    /// which only the merge can judge.
    pub fn parse(s: &str) -> Result<ShellProfilesFile, ShellProfileError> {
        let raw: RawFile =
            toml::from_str(s).map_err(|e| ShellProfileError::Toml(e.message().to_owned()))?;
        let shells: Vec<ShellProfile> = raw
            .shell
            .into_iter()
            .map(validate)
            .collect::<Result<_, _>>()?;
        for (n, s) in shells.iter().enumerate() {
            if shells[..n].iter().any(|o| o.name == s.name) {
                return Err(ShellProfileError::DuplicateName {
                    name: s.name.clone(),
                });
            }
        }
        Ok(ShellProfilesFile {
            default: raw.default,
            shells,
        })
    }

    /// Merges the layers' files, in ASCENDING precedence: a higher layer
    /// replaces a lower one's shell profile of the same name in place, and
    /// the highest `default` wins. No shell profile at all is
    /// [`Self::implicit`].
    ///
    /// # Errors
    /// [`ShellProfileError::UnknownDefault`] if the winning `default` names
    /// no shell profile — said, not silently replaced by another shell.
    pub fn merge(files: Vec<ShellProfilesFile>) -> Result<Self, ShellProfileError> {
        let mut shells: Vec<ShellProfile> = Vec::new();
        let mut default = None;
        for f in files {
            if f.default.is_some() {
                default = f.default;
            }
            for s in f.shells {
                match shells.iter_mut().find(|o| o.name == s.name) {
                    Some(old) => *old = s,
                    None => shells.push(s),
                }
            }
        }
        let default = match default {
            Some(name) => shells
                .iter()
                .position(|s| s.name == name)
                .ok_or(ShellProfileError::UnknownDefault { name })?,
            None if shells.is_empty() => return Ok(Self::implicit()),
            None => 0,
        };
        Ok(Self { shells, default })
    }

    /// The one `+` starts.
    #[must_use]
    pub fn default_profile(&self) -> &ShellProfile {
        // Non-empty by construction (`implicit` or a merge that found one),
        // and `default` is a position inside it.
        &self.shells[self.default]
    }

    /// By name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&ShellProfile> {
        self.shells.iter().find(|s| s.name == name)
    }

    /// All of them, in declaration order.
    pub fn iter(&self) -> impl Iterator<Item = &ShellProfile> {
        self.shells.iter()
    }
}

fn validate(r: RawShell) -> Result<ShellProfile, ShellProfileError> {
    let name = r.name.trim().to_owned();
    if name.is_empty() {
        return Err(ShellProfileError::EmptyName);
    }
    let program = PathBuf::from(r.program);
    if !program.is_absolute() {
        return Err(ShellProfileError::RelativeProgram { name });
    }
    let icon = match r.icon {
        None => None,
        Some(v) => Some(
            TerminalIcon::parse(&v).ok_or_else(|| ShellProfileError::BadIcon {
                name: name.clone(),
                value: v,
            })?,
        ),
    };
    let color = match r.color {
        None => None,
        Some(value) => Some(
            AnsiColor::new(value).ok_or_else(|| ShellProfileError::BadColor {
                name: name.clone(),
                value,
            })?,
        ),
    };
    Ok(ShellProfile {
        name,
        program,
        args: r.args.into_iter().map(OsString::from).collect(),
        icon,
        color,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_relative_program_is_refused() {
        let e = ShellProfiles::parse("[[shell]]\nname = \"x\"\nprogram = \"fish\"\n").unwrap_err();
        assert!(matches!(e, ShellProfileError::RelativeProgram { .. }));
    }

    #[test]
    fn an_empty_name_is_refused() {
        let e =
            ShellProfiles::parse("[[shell]]\nname = \" \"\nprogram = \"/bin/sh\"\n").unwrap_err();
        assert!(matches!(e, ShellProfileError::EmptyName));
    }

    /// Across layers a name overrides; inside ONE file a repeat is a typo.
    #[test]
    fn a_name_repeated_in_one_file_is_refused() {
        let e = ShellProfiles::parse(
            "[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\n[[shell]]\nname = \"a\"\nprogram = \"/bin/bash\"\n",
        )
        .unwrap_err();
        assert!(matches!(e, ShellProfileError::DuplicateName { .. }));
    }

    #[test]
    fn an_unknown_default_is_refused_not_ignored() {
        let f = ShellProfiles::parse(
            "default = \"nope\"\n[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\n",
        )
        .expect("parses");
        assert!(matches!(
            ShellProfiles::merge(vec![f]),
            Err(ShellProfileError::UnknownDefault { .. })
        ));
    }

    #[test]
    fn colour_and_icon_are_checked() {
        assert!(matches!(
            ShellProfiles::parse("[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\ncolor = 9\n"),
            Err(ShellProfileError::BadColor { .. })
        ));
        assert!(matches!(
            ShellProfiles::parse(
                "[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\nicon = \"rocket\"\n"
            ),
            Err(ShellProfileError::BadIcon { .. })
        ));
    }

    #[test]
    fn unknown_keys_are_refused() {
        assert!(matches!(
            ShellProfiles::parse("[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\nenv = 1\n"),
            Err(ShellProfileError::Toml(_))
        ));
    }

    #[test]
    fn the_higher_layer_wins_on_equal_name() {
        let low = ShellProfiles::parse(
            "default = \"a\"\n[[shell]]\nname = \"a\"\nprogram = \"/bin/sh\"\n[[shell]]\nname = \"b\"\nprogram = \"/bin/sh\"\n",
        )
        .expect("low");
        let high = ShellProfiles::parse(
            "[[shell]]\nname = \"a\"\nprogram = \"/bin/bash\"\nargs = [\"-l\"]\n",
        )
        .expect("high");
        let m = ShellProfiles::merge(vec![low, high]).expect("merges");
        let a = m.get("a").expect("a");
        assert_eq!(a.program, PathBuf::from("/bin/bash"));
        assert_eq!(a.args, vec![std::ffi::OsString::from("-l")]);
        assert_eq!(
            m.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
            ["a", "b"],
            "replaced in place, order kept"
        );
        assert_eq!(
            m.default_profile().name,
            "a",
            "default from the layer that set it"
        );
    }

    #[test]
    fn without_a_default_the_first_one_is() {
        let f = ShellProfiles::parse(
            "[[shell]]\nname = \"z\"\nprogram = \"/bin/sh\"\nicon = \"server\"\ncolor = 2\n",
        )
        .expect("parses");
        let m = ShellProfiles::merge(vec![f]).expect("merges");
        let d = m.default_profile();
        assert_eq!(d.name, "z");
        assert_eq!(d.icon, Some(TerminalIcon::Server));
        assert_eq!(d.color.map(AnsiColor::index), Some(2));
    }

    #[test]
    fn no_file_means_the_login_shell() {
        let m = ShellProfiles::merge(Vec::new()).expect("merges");
        assert_eq!(m.iter().count(), 1);
        assert!(m.default_profile().program.is_absolute());
        assert!(!m.default_profile().name.is_empty());
    }
}
