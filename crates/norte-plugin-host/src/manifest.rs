//! `plugin.toml` manifest (ADR 0022 D3): identity + per-interface
//! contributions (`VSCode`-style `contributes`) + capabilities + `[config]`
//! (P2: typed settings declared by the plugin).

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::capability::Capabilities;

/// The plugin's PRIMARY category = the WIT interface it is ordered by in
/// the manager (spec §7.1). A plugin can contribute to several, but
/// declares one main one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Category {
    /// Generates previews for mimetypes.
    Previewer,
    /// Third-party VFS provider.
    Provider,
    /// Command invokable from the palette/keybinding.
    Command,
    /// Custom columns in the listing.
    Columns,
    /// Observes mutations the journal already recorded (H1, ADR 0100, WIT
    /// interface `hook` of package `norte:hook`, world `norte-hook`). Only
    /// `after-*`: a hook neither vetoes nor mutates, and its only effect is
    /// a sentence for the human. The events it listens to go in
    /// `[[contributions.hook]]`, from the [`HOOK_EVENTS`] vocabulary.
    Hook,
    /// Decorates visible entries with a "git status"-like badge/role (ADR
    /// 0037 decision 2, WIT interface `decorator`, world
    /// `norte-decorator`).
    Decorator,
    /// Proposes rename pairs for a batch (C3, ADR 0095, WIT interface
    /// `renamer` of package `norte:renamer`, world `norte-renamer`). The
    /// core runs them through the same path as the AI's plan.
    Renamer,
    /// Builds a THUMBNAIL of a file for the window's viewer (ADR 0107,
    /// package `norte:thumbnail`, world `norte-thumbnail`). The mimetypes
    /// it handles go in `[[contributions.thumbnail]]`, like a previewer's;
    /// it receives capped bytes and returns a raster the host verifies
    /// before painting it.
    Thumbnail,
    /// Paints an entire PANEL of the layout (phase 3 of the 2026-09-15
    /// program, package `norte:panel`, world `norte-panel`). The panels it
    /// supplies go in `[[contributions.panel]]`, each with its `kind` and
    /// its minimum size; the slot is named `plugin:<id>:<kind>` and cannot
    /// collide with a built-in one. The guest describes styled lines and
    /// clickable zones that name catalog COMMANDS: a click on one cannot
    /// do anything the reader could not do with a key.
    Panel,
    /// Proposes WHERE to MOVE each file, with subdirectories (phase 8,
    /// package `norte:organizer`, world `norte-organizer`). Generalizes
    /// [`Category::Renamer`]: there the destination is a name, here a
    /// relative path, so the plan also creates folders. The organizers it
    /// supplies go in `[[contributions.organizer]]`, and the core runs
    /// them through the same path as the AI's plan — with the same
    /// destination validation, which is what prevents writing outside the
    /// directory.
    Organizer,
}

impl Category {
    /// Stable name (for grouping in the UI and traces).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Previewer => "previewer",
            Category::Provider => "provider",
            Category::Command => "command",
            Category::Columns => "columns",
            Category::Hook => "hook",
            Category::Decorator => "decorator",
            Category::Renamer => "renamer",
            Category::Thumbnail => "thumbnail",
            Category::Panel => "panel",
            Category::Organizer => "organizer",
        }
    }

    /// Canonical, stable byte for the approval digest (issue #69). Does
    /// NOT use the enum's discriminant (it could be reordered) but a fixed
    /// value. `Decorator` = 5 (ADR 0037): a NEW value at the end, it never
    /// reuses nor reorders the existing ones — digests of manifests that
    /// predate this category are not affected by its mere existence.
    fn digest_tag(self) -> u8 {
        match self {
            Category::Previewer => 0,
            Category::Provider => 1,
            Category::Command => 2,
            Category::Columns => 3,
            Category::Hook => 4,
            Category::Decorator => 5,
            // New at the end (ADR 0095), like `Decorator` in its day.
            Category::Renamer => 6,
            // And the next one after it (ADR 0107): a tag is forever.
            Category::Thumbnail => 7,
            // And the next one after it, for the same reason: manifests
            // already approved cannot move just because one more category
            // exists.
            Category::Panel => 8,
            // And the next one after it (phase 8). A tag is FOREVER:
            // already-approved manifests cannot change digest just
            // because one more category exists.
            Category::Organizer => 9,
        }
    }
}

/// A declared previewer: the mimetypes it can paint.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreviewerContrib {
    /// Mimetype patterns (`text/*`, `application/json`).
    pub mimetypes: Vec<String>,
}

/// A declared command.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommandContrib {
    /// Stable id (namespaced by the plugin when registering it).
    pub id: String,
    /// Title for the palette.
    pub title: String,
}

/// A declared column.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColumnContrib {
    /// Stable id.
    pub id: String,
    /// Visible header.
    pub header: String,
}

/// A declared PANEL (phase 3 of the 2026-09-15 program): a layout slot
/// whose content the guest paints.
///
/// The `kind` is stable within the plugin and the slot ends up named
/// `plugin:<id>:<kind>`, which is what prevents it colliding with a
/// built-in one. The minimum size is declared by the plugin because it is
/// the one that knows it — a two-column panel says nothing — and the
/// layout collapses the `Split` that contains it when it does not fit,
/// same as with any kind.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PanelContrib {
    /// Stable id within the plugin.
    pub kind: String,
    /// Readable title. Plugin text — NOT trusted.
    pub title: String,
    /// Minimum width in cells, if the panel asks for one.
    #[serde(default, rename = "min-cols")]
    pub min_cols: Option<u16>,
    /// Minimum height in cells, if the panel asks for one.
    #[serde(default, rename = "min-rows")]
    pub min_rows: Option<u16>,
}

/// A declared provider: the scheme it serves (`webdav`, …).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderContrib {
    /// VFS scheme (without `://`).
    pub scheme: String,
    /// Port the guest connects to when the URL does not say (`default-port
    /// = 8443`). A provider plugin receives network access to `ip:port`,
    /// never to the whole IP, and the host does not know a foreign
    /// scheme's default port: without this field and without a port in
    /// the URL, the connection is refused. Goes into the approval digest
    /// like the scheme.
    #[serde(default, rename = "default-port")]
    pub default_port: Option<u16>,
}

/// A declared thumbnail maker (ADR 0107): the mimetypes it can convert
/// into a raster, with the same matching rules as a previewer's (exact
/// before wildcard).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThumbnailContrib {
    /// Mimetypes it handles (`image/png`, `image/*`).
    pub mimetypes: Vec<String>,
}

/// A declared renamer (C3, ADR 0095): a name proposer with its id and its
/// title. A plugin can supply several ("by EXIF date", "by ID3 title");
/// the id is what travels to `renamer.plan` and the title is what the
/// palette shows.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RenamerContrib {
    /// Stable id within the plugin.
    pub id: String,
    /// Readable title. Plugin text — NOT trusted.
    pub title: String,
}

/// A declared organizer (phase 8): a REORGANIZATION proposer with its id
/// and its title. Same shape as [`RenamerContrib`] because it plays the
/// same role; what changes is what it proposes — a relative path instead
/// of a name — and that lives in the WIT, not here.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OrganizerContrib {
    /// Stable id within the plugin.
    pub id: String,
    /// Readable title. Plugin text — NOT trusted.
    pub title: String,
}

/// A declared hook: the event it hooks into.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookContrib {
    /// Event, one of [`HOOK_EVENTS`] (`after-renamed`, …). Closed and
    /// validated while parsing: a value not in it is
    /// [`ManifestError::HookUnknownEvent`], like an unknown capability.
    pub on: String,
}

/// How many sidecars a hook can declare. Sixteen is "several work files";
/// above that it is a plugin that wants a directory, and that is a
/// different capability.
pub const SIDECAR_MAX_NAMES: usize = 16;

/// Byte cap for a sidecar's name: `NAME_MAX` on common filesystems.
const SIDECAR_NAME_MAX_BYTES: usize = 255;

/// `true` if `name` is A PORTABLE file name the host will agree to write
/// next to an event: printable ASCII without `/ \ : * ? " < > |`, not
/// empty, fits in `NAME_MAX`, not `.` nor `..`, no trailing dot and not a
/// reserved Windows name (`CON`, `NUL`, `COM1`…). ASCII because the name is
/// third-party text that gets painted in the approval and written to disk:
/// no bidi, no invisibles, no homoglyphs.
///
/// ```
/// use norte_plugin_host::is_valid_sidecar_name;
/// assert!(is_valid_sidecar_name(".norte-renames.log"));
/// assert!(!is_valid_sidecar_name("a/b"));
/// assert!(!is_valid_sidecar_name(".."));
/// assert!(!is_valid_sidecar_name("CON"));
/// assert!(!is_valid_sidecar_name("log\u{202e}"));
/// ```
#[must_use]
pub fn is_valid_sidecar_name(name: &str) -> bool {
    const FORBIDDEN: &[u8] = b"/\\:*?\"<>|";
    const RESERVED: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if name.is_empty() || name.len() > SIDECAR_NAME_MAX_BYTES || name == "." || name == ".." {
        return false;
    }
    if !name
        .bytes()
        .all(|b| (0x21..=0x7e).contains(&b) && !FORBIDDEN.contains(&b))
    {
        return false;
    }
    if name.ends_with('.') {
        return false;
    }
    let stem = name.split('.').next().unwrap_or(name).to_ascii_uppercase();
    !RESERVED.contains(&stem.as_str())
}

/// The CLOSED vocabulary of `[[contributions.hook]].on` (ADR 0100): journal
/// operations, in the past tense, because a hook only sees what has
/// already been recorded. There is no `before-*` — that would be policy,
/// not a plugin.
///
/// Lives here and not in the core because the three pieces that must
/// agree — whoever validates it (this crate), whoever fires it
/// (`norte-core`) and the guide — all start from a list; without it, each
/// one keeps its own copy.
pub const HOOK_EVENTS: &[&str] = &[
    "after-created",
    "after-removed",
    "after-trashed",
    "after-renamed",
    "after-mode-changed",
];

/// A declared decorator (ADR 0037 decision 2): an EMPTY marker — unlike
/// [`PreviewerContrib`]/[`ColumnContrib`], a decorator declares no
/// mimetypes or ids: the WIT interface `decorator::decorate` is called for
/// EVERY visible entry on the page (batched, with no prior type filter).
/// The entry exists (rather than letting `category = "decorator"` be
/// enough on its own) to leave symmetrical room for future fields (e.g. an
/// exclusion glob) without another shape change to the manifest; today it
/// is deliberately `{}` — `deny_unknown_fields` so an unknown hostile
/// field rejects the manifest instead of being ignored silently.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DecoratorContrib {
    /// Which SLOT of the row this decorator's return value is painted in
    /// (ADR 0105): `badge` (default) to the right of the name, like a git
    /// status; `icon` to the left, in a fixed-width column. The two slots
    /// coexist: an icon and a badge on the same row come from two
    /// different plugins. Goes into the digest only when it is not the
    /// default value, so that no earlier manifest changes anchor.
    #[serde(default)]
    pub slot: DecoratorSlot,
}

/// The row slot a decorator fills (ADR 0105).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DecoratorSlot {
    /// To the right of the name, short text: `M`, `++`.
    #[default]
    Badge,
    /// To the left of the name, one glyph per row.
    Icon,
}

/// What the plugin CONTRIBUTES, per interface. All optional: a
/// single-interface plugin fills in only its own.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Contributions {
    /// Previewers.
    #[serde(default)]
    pub previewer: Vec<PreviewerContrib>,
    /// Commands.
    #[serde(default)]
    pub command: Vec<CommandContrib>,
    /// Columns.
    #[serde(default)]
    pub columns: Vec<ColumnContrib>,
    /// Providers.
    #[serde(default)]
    pub provider: Vec<ProviderContrib>,
    /// Hooks.
    #[serde(default)]
    pub hook: Vec<HookContrib>,
    /// Decorators (ADR 0037 decision 2). Unlike the other sections, this
    /// one does NOT go into `Contributions::update_digest`
    /// (unconditionally): it follows `[config]`'s OPTIONAL pattern — see
    /// `update_decorator_digest` — so that a manifest without
    /// `[[contributions.decorator]]` digests EXACTLY like it did before
    /// this category (existing human approvals are not reset just because
    /// this field exists).
    #[serde(default)]
    pub decorator: Vec<DecoratorContrib>,
    /// Renamers (C3, ADR 0095). Additive with a default, like the others:
    /// earlier manifests don't carry it and its digest does not move.
    #[serde(default)]
    pub renamer: Vec<RenamerContrib>,
    /// Organizers (phase 8). Additive with a default, like the others.
    #[serde(default)]
    pub organizer: Vec<OrganizerContrib>,
    /// Panels the plugin paints (phase 3 of the 2026-09-15 program).
    /// Follows the digest's OPTIONAL pattern, like `decorator` and
    /// `[config]`: a manifest without `[[contributions.panel]]` digests
    /// byte for byte the same as before this category existed, so
    /// approvals the reader already gave are not reset just because the
    /// field appears.
    #[serde(default)]
    pub panel: Vec<PanelContrib>,
    /// Thumbnail makers (ADR 0107).
    #[serde(default)]
    pub thumbnail: Vec<ThumbnailContrib>,
}

impl Contributions {
    /// Feeds a hasher with the CANONICAL form of the contributions,
    /// WITHOUT finalizing (issue #69). These are the fields that decide
    /// WHEN/HOW the plugin fires (a previewer's mimetypes, command ids, a
    /// provider's schemes, hook events): changing them while keeping the
    /// same capabilities must NOT preserve the approval (otherwise a
    /// `command` re-edited to `previewer` would start auto-running in the
    /// viewer over matching files). Order is preserved (not sorted): a
    /// reorder triggers re-consent — conservative and fail-closed. Each
    /// section goes with its entry count and each string is
    /// length-prefixed (no ambiguity between sections).
    fn update_digest(&self, h: &mut sha2::Sha256) {
        use crate::capability::update_str;
        use sha2::Digest;

        h.update((self.previewer.len() as u64).to_le_bytes());
        for c in &self.previewer {
            h.update((c.mimetypes.len() as u64).to_le_bytes());
            for m in &c.mimetypes {
                update_str(h, m);
            }
        }
        h.update((self.command.len() as u64).to_le_bytes());
        for c in &self.command {
            update_str(h, &c.id);
            update_str(h, &c.title);
        }
        h.update((self.columns.len() as u64).to_le_bytes());
        for c in &self.columns {
            update_str(h, &c.id);
            update_str(h, &c.header);
        }
        h.update((self.provider.len() as u64).to_le_bytes());
        for c in &self.provider {
            update_str(h, &c.scheme);
            // Presence + value, like any optional field in the digest: the
            // port decides what network access is granted, so changing it
            // after approval is changing what was approved.
            h.update([u8::from(c.default_port.is_some())]);
            h.update(c.default_port.unwrap_or_default().to_le_bytes());
        }
        h.update((self.hook.len() as u64).to_le_bytes());
        for c in &self.hook {
            update_str(h, &c.on);
        }
        // Renamers, AFTER everything and only if there are any: a
        // manifest with none digests exactly what it digested before they
        // existed, and no approval is reset by their mere introduction.
        // With some, a fixed separator and each id/title pair, like
        // commands: changing what a plugin proposes is changing what was
        // approved.
        if !self.renamer.is_empty() {
            h.update(b"renamer:\n");
            h.update((self.renamer.len() as u64).to_le_bytes());
            for c in &self.renamer {
                update_str(h, &c.id);
                update_str(h, &c.title);
            }
        }
        // Thumbnails, with the same treatment (ADR 0107): after
        // everything and only if there are any, so no existing manifest
        // changes digest.
        if !self.thumbnail.is_empty() {
            h.update(b"thumbnail:\n");
            h.update((self.thumbnail.len() as u64).to_le_bytes());
            for c in &self.thumbnail {
                h.update((c.mimetypes.len() as u64).to_le_bytes());
                for m in &c.mimetypes {
                    update_str(h, m);
                }
            }
        }
        // And panels, with the same treatment: after everything and only
        // if there are any, so no existing manifest changes digest just
        // because the category appears. In goes the `kind` (it is the
        // slot the plugin occupies), the title (what the reader reads in
        // the bar) and the minimums: a panel that after approval asks for
        // half the screen is no longer the panel that was approved.
        if !self.panel.is_empty() {
            h.update(b"panel:\n");
            h.update((self.panel.len() as u64).to_le_bytes());
            for c in &self.panel {
                update_str(h, &c.kind);
                update_str(h, &c.title);
                // Presence + value, like a provider's port: "no minimum"
                // and "minimum zero" are not the same.
                h.update([u8::from(c.min_cols.is_some())]);
                h.update(c.min_cols.unwrap_or_default().to_le_bytes());
                h.update([u8::from(c.min_rows.is_some())]);
                h.update(c.min_rows.unwrap_or_default().to_le_bytes());
            }
        }
        // And organizers, after everything and only if there are any
        // (phase 8), for the same reason as their five predecessors: a
        // manifest with none digests byte for byte what it digested
        // before, so no human approval is reset just because this
        // category exists.
        if !self.organizer.is_empty() {
            h.update(b"organizer:\n");
            h.update((self.organizer.len() as u64).to_le_bytes());
            for c in &self.organizer {
                update_str(h, &c.id);
                update_str(h, &c.title);
            }
        }
    }
}

/// A `[config.<key>]` key of the manifest (P2), already validated: the
/// TOML `type` fixes the exact shape (mirroring [`Capabilities`]'s
/// style — an absent permission/non-applicable field simply does not
/// exist in the variant). `description` is cosmetic for the manager's UI
/// and is deliberately OUTSIDE [`Manifest::approval_digest`] (same
/// treatment as [`Manifest::description`]): editing it does not
/// reinvalidate already-approved capabilities, because it does not change
/// what values the key can take.
#[derive(Debug, Clone, PartialEq)]
pub enum ConfigKeySpec {
    /// `type = "string"`.
    String {
        /// Default value (cap [`CONFIG_STRING_MAX_CHARS`] characters).
        default: String,
        /// Cosmetic text for the UI (cap
        /// [`CONFIG_DESCRIPTION_MAX_CHARS`] characters). Does NOT go into
        /// the approval digest.
        description: Option<String>,
    },
    /// `type = "bool"`.
    Bool {
        /// Default value.
        default: bool,
        /// See [`ConfigKeySpec::String::description`].
        description: Option<String>,
    },
    /// `type = "int"`.
    Int {
        /// Default value; MUST fall within `[min, max]` when declared
        /// (validated while parsing, fail-loud).
        default: i64,
        /// Inclusive lower bound (optional).
        min: Option<i64>,
        /// Inclusive upper bound (optional).
        max: Option<i64>,
        /// See [`ConfigKeySpec::String::description`].
        description: Option<String>,
    },
    /// `type = "enum"`.
    Enum {
        /// Default value; MUST be in `values` (validated while parsing,
        /// fail-loud).
        default: String,
        /// Allowed values (cap [`CONFIG_ENUM_MAX_VALUES`] entries, each up
        /// to [`CONFIG_STRING_MAX_CHARS`] characters).
        values: Vec<String>,
        /// See [`ConfigKeySpec::String::description`].
        description: Option<String>,
    },
}

impl ConfigKeySpec {
    /// Canonical, stable byte for the approval digest (same criterion as
    /// [`Category::digest_tag`]/`Scope::digest_tag`): does NOT use the
    /// enum's discriminant (it could be reordered) but a fixed value.
    fn digest_tag(&self) -> u8 {
        match self {
            ConfigKeySpec::String { .. } => 0,
            ConfigKeySpec::Bool { .. } => 1,
            ConfigKeySpec::Int { .. } => 2,
            ConfigKeySpec::Enum { .. } => 3,
        }
    }

    /// Feeds a hasher with this key's CANONICAL form, WITHOUT finalizing
    /// (composes [`Manifest::approval_digest`]): type tag + the fields
    /// that affect behavior (`default`, `min`, `max`, `values`).
    /// `description` is EXCLUDED on purpose (cosmetic, see the type's
    /// doc).
    fn update_digest(&self, h: &mut sha2::Sha256) {
        use crate::capability::{update_opt_i64, update_str};
        use sha2::Digest;
        h.update([self.digest_tag()]);
        match self {
            ConfigKeySpec::String { default, .. } => update_str(h, default),
            ConfigKeySpec::Bool { default, .. } => h.update([u8::from(*default)]),
            ConfigKeySpec::Int {
                default, min, max, ..
            } => {
                h.update(default.to_le_bytes());
                update_opt_i64(h, *min);
                update_opt_i64(h, *max);
            }
            ConfigKeySpec::Enum {
                default, values, ..
            } => {
                update_str(h, default);
                // `values` is an ORDERED LIST (not a set): the order the
                // user declares them in is the order shown in the UI
                // (ADR-style: same criterion as `Contributions`, which
                // also does not sort). Changing the order DOES move the
                // digest.
                h.update((values.len() as u64).to_le_bytes());
                for v in values {
                    update_str(h, v);
                }
            }
        }
    }
}

/// Raw form of a `[config.<key>]` entry (before validation). The `type`
/// field (`serde(tag = "type")`) selects the variant; TOML is
/// self-describing so the internal tag works unambiguously.
#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case", deny_unknown_fields)]
enum ConfigKeyRaw {
    /// `type = "string"`.
    String {
        default: String,
        #[serde(default)]
        description: Option<String>,
    },
    /// `type = "bool"`.
    Bool {
        default: bool,
        #[serde(default)]
        description: Option<String>,
    },
    /// `type = "int"`.
    Int {
        default: i64,
        #[serde(default)]
        min: Option<i64>,
        #[serde(default)]
        max: Option<i64>,
        #[serde(default)]
        description: Option<String>,
    },
    /// `type = "enum"`.
    Enum {
        default: String,
        values: Vec<String>,
        #[serde(default)]
        description: Option<String>,
    },
}

/// Cap on the number of `[config]` keys (P2 decision 1).
pub const CONFIG_MAX_KEYS: usize = 32;
/// Length cap for a `[config.<key>]` key; the allowed charset is
/// `[a-z0-9-]{1,32}` (P2 decision 1) — no uppercase, no `_`, no non-ASCII,
/// so the key is safe to interpolate into values TOML
/// (`config_dir/plugins/<id>/config.toml`), logs and the manager's UI
/// without escaping.
pub const CONFIG_KEY_MAX_CHARS: usize = 32;
/// Cap on `[config.<key>].description` (P2 decision 1), same criterion as
/// [`Manifest::description`] (280 CHARACTERS, not bytes).
pub const CONFIG_DESCRIPTION_MAX_CHARS: usize = 280;
/// Cap on a `string`-typed `default`, or each `values` entry (enum) (P2
/// decision 1), in CHARACTERS.
pub const CONFIG_STRING_MAX_CHARS: usize = 280;
/// Cap on entries in `[config.<key>].values` (enum) (P2 decision 1).
pub const CONFIG_ENUM_MAX_VALUES: usize = 16;

/// `true` if `key` respects the `[a-z0-9-]{1,32}` charset (P2 decision 1):
/// only lowercase ASCII, digits and hyphen, length `1..=32`.
/// `pub(crate)`: reused by `config_values.rs` (security review P2 Task 4a)
/// to decide whether an UNKNOWN key from `config.toml` is safe to
/// interpolate into an error message — the same bounded charset (ASCII, no
/// control/bidi, length cap) that already guarantees every key DECLARED in
/// the schema.
pub(crate) fn is_valid_config_key(key: &str) -> bool {
    !key.is_empty()
        && key.len() <= CONFIG_KEY_MAX_CHARS
        && key
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Validates a raw `[config.<key>]` entry against its own type caps and
/// returns the already-validated form. `key` is NOT used in error
/// messages (same criterion as `Id`/`DuplicateId`... except that here not
/// even the plugin id, already validated, is risked: a config key can
/// come from ANY hostile TOML before it passes the charset).
fn validate_config_entry(raw: ConfigKeyRaw) -> Result<ConfigKeySpec, ManifestError> {
    fn check_description(description: Option<&String>) -> Result<(), ManifestError> {
        if description.is_some_and(|d| d.chars().count() > CONFIG_DESCRIPTION_MAX_CHARS) {
            return Err(ManifestError::ConfigDescriptionTooLong);
        }
        Ok(())
    }

    match raw {
        ConfigKeyRaw::String {
            default,
            description,
        } => {
            check_description(description.as_ref())?;
            if default.chars().count() > CONFIG_STRING_MAX_CHARS {
                return Err(ManifestError::ConfigDefaultTooLong);
            }
            Ok(ConfigKeySpec::String {
                default,
                description,
            })
        }
        ConfigKeyRaw::Bool {
            default,
            description,
        } => {
            check_description(description.as_ref())?;
            Ok(ConfigKeySpec::Bool {
                default,
                description,
            })
        }
        ConfigKeyRaw::Int {
            default,
            min,
            max,
            description,
        } => {
            check_description(description.as_ref())?;
            if min.is_some_and(|m| default < m) || max.is_some_and(|m| default > m) {
                return Err(ManifestError::ConfigIntDefaultOutOfRange);
            }
            Ok(ConfigKeySpec::Int {
                default,
                min,
                max,
                description,
            })
        }
        ConfigKeyRaw::Enum {
            default,
            values,
            description,
        } => {
            check_description(description.as_ref())?;
            if values.len() > CONFIG_ENUM_MAX_VALUES {
                return Err(ManifestError::ConfigEnumTooManyValues);
            }
            if values
                .iter()
                .any(|v| v.chars().count() > CONFIG_STRING_MAX_CHARS)
            {
                return Err(ManifestError::ConfigEnumValueTooLong);
            }
            if !values.iter().any(|v| v == &default) {
                return Err(ManifestError::ConfigEnumDefaultNotInValues);
            }
            Ok(ConfigKeySpec::Enum {
                default,
                values,
                description,
            })
        }
    }
}

/// Feeds a hasher with the CANONICAL form of the whole `[config]`, WITHOUT
/// finalizing (P2 decision 2): the `config:` section is ONLY added if
/// `config` is NOT empty — a manifest without `[config]` (or with a table
/// present but no keys) digests THE SAME as before P2, so existing human
/// approvals of plugins that do not use `[config]` are NEVER reset. The
/// `BTreeMap` already iterates in key order (deterministic, independent of
/// the order in the file).
fn update_config_digest(config: &BTreeMap<String, ConfigKeySpec>, h: &mut sha2::Sha256) {
    use crate::capability::update_str;
    use sha2::Digest;
    if config.is_empty() {
        return;
    }
    // FIXED domain separator (not interpolated, not ambiguous with user
    // content): marks where the optional section begins.
    h.update(b"config:\n");
    h.update((config.len() as u64).to_le_bytes());
    for (key, spec) in config {
        update_str(h, key);
        spec.update_digest(h);
    }
}

/// Feeds a hasher with the CANONICAL form of `contributions.decorator`,
/// WITHOUT finalizing (ADR 0037 decision 2): the same OPTIONAL pattern as
/// [`update_config_digest`] — the `decorator:` section is ONLY added if
/// the `Vec` is NOT empty, so a manifest without
/// `[[contributions.decorator]]` (the vast majority, including EVERY
/// manifest that existed before this category) digests EXACTLY the same
/// as before this change — no existing human approval is reset just
/// because the field exists. A manifest that DOES declare at least one
/// decorator moves the digest (forces consent) because moving to
/// `category = "decorator"` radically changes when/how the plugin fires.
fn update_decorator_digest(decorator: &[DecoratorContrib], h: &mut sha2::Sha256) {
    use sha2::Digest;
    if decorator.is_empty() {
        return;
    }
    // FIXED domain separator, same criterion as `update_config_digest`.
    h.update(b"decorator:\n");
    h.update((decorator.len() as u64).to_le_bytes());
    // The slot (ADR 0105) ONLY when it is not the usual one: a manifest
    // predating the icon column digests byte for byte the same as before,
    // and moving to `icon` moves the anchor because it changes where the
    // plugin paints. WITH its position: the core reads the FIRST
    // contribution's slot, and without the index, reordering two blocks
    // would move an approved plugin from the badge to the icon column
    // without the digest noticing.
    for (i, d) in decorator.iter().enumerate() {
        if d.slot == DecoratorSlot::Icon {
            h.update(b"slot:icon@");
            h.update((i as u64).to_le_bytes());
        }
    }
}

/// Manifest's `[plugin]` block.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginSection {
    id: String,
    name: String,
    publisher: String,
    version: String,
    category: Category,
    /// Cosmetic description (P1); absent = `None`. 280-char cap in
    /// [`Manifest::from_toml`] — see [`Manifest::description`].
    #[serde(default)]
    description: Option<String>,
}

/// Raw form of the TOML (before validation).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestRaw {
    plugin: PluginSection,
    #[serde(default)]
    contributions: Contributions,
    #[serde(default)]
    capabilities: Capabilities,
    /// `[config.<key>]` (P2); absent = empty map. `BTreeMap` so the
    /// iteration order is deterministic regardless of the order in the
    /// file (relevant to [`Manifest::approval_digest`]).
    #[serde(default)]
    config: BTreeMap<String, ConfigKeyRaw>,
}

/// A plugin's already-validated manifest.
#[derive(Debug, Clone)]
pub struct Manifest {
    /// Unique reverse-DNS id (`org.norte.syntax-preview`).
    pub id: String,
    /// Readable name.
    pub name: String,
    /// Publisher.
    pub publisher: String,
    /// Version (`SemVer`, not validated here).
    pub version: String,
    /// Primary category (orders the manager).
    pub category: Category,
    /// Cosmetic description (P1), 280-character cap (fail-loud while
    /// parsing, like `id`). `None` if the manifest does not declare it.
    /// Text supplied by the plugin — NOT trusted, a frontend must mask it
    /// before rendering. Deliberately OUTSIDE
    /// [`Manifest::approval_digest`] (same treatment as
    /// `name`/`publisher`/`version`): editing it does not reinvalidate
    /// capabilities the human already approved, because it does not
    /// change what the plugin does nor when it fires.
    pub description: Option<String>,
    /// Per-interface contributions.
    pub contributions: Contributions,
    /// Declared capabilities.
    pub capabilities: Capabilities,
    /// Typed settings the plugin declares (P2), already validated.
    /// `BTreeMap` for deterministic order by key. Absent `[config]` in the
    /// TOML ⇒ empty map. DOES go into [`Manifest::approval_digest`]
    /// (affects behavior: defines what values each setting can take),
    /// except each key's `description` (cosmetic, same as
    /// [`Manifest::description`]).
    pub config: BTreeMap<String, ConfigKeySpec>,
}

/// Error loading a manifest — or, more broadly, the reason a plugin
/// candidate is excluded from the catalog (same type as
/// [`crate::LoadError::error`]): besides parsing/validating `plugin.toml`
/// itself, it covers catalog-level conditions such as `DuplicateId` and,
/// since P2, `ConfigValues` (a VALUES `config.toml` that fails to validate
/// against the `[config]` schema — the whole plugin is excluded, not just
/// the offending key).
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    /// The TOML does not parse or has unknown keys.
    #[error("invalid plugin.toml: {0}")]
    Toml(#[from] toml::de::Error),
    /// `plugin.id` empty or not reverse-DNS (no `.`).
    #[error("invalid plugin.id: reverse-DNS expected (e.g. `org.foo.bar`)")]
    Id,
    /// `exec` other than `none`: FORBIDDEN (spec §7.1, hard invariant).
    #[error("capability `exec` forbidden: must be `none` (or absent)")]
    ExecForbidden,
    /// `location-root-marker` that is not A name: empty, with a separator,
    /// with NUL, `.`/`..`, or absurdly long. A marker with a slash inside
    /// would make the host search by a PATH going up, which is a
    /// different capability.
    #[error("`location-root-marker` must be a simple name (no `/`, no NUL, not `.`/`..`)")]
    LocationMarker,
    /// `location-root-marker` declared WITHOUT `location = "read"`: it
    /// would ask to open an ancestor without requesting the capability
    /// that reads it. Rejected instead of ignored, so the author finds
    /// out.
    #[error(
        "`location-root-marker` without `location = \"read\"`: declare the capability or remove the marker"
    )]
    LocationMarkerWithoutCap,
    /// `[[contributions.hook]].on` with a value outside [`HOOK_EVENTS`].
    /// The vocabulary is closed on purpose: a `before-copy` that got
    /// accepted would install a hook that never fires, and its author
    /// would find out because nothing ever happens. Carries the value so
    /// the error is actionable.
    #[error(
        "unknown hook event `{0}`: the ones that exist are after-created, after-removed, after-trashed, after-renamed and after-mode-changed"
    )]
    HookUnknownEvent(String),
    /// `category = "hook"` with no `[[contributions.hook]]` at all: a
    /// plugin that claims to observe and listens to nothing is inert, and
    /// the manager would paint it as a normal one.
    #[error(
        "`category = \"hook\"` with no `[[contributions.hook]]`: declare which events it listens to"
    )]
    HookWithoutEvents,
    /// `[[contributions.hook]]` on a plugin of another category: only
    /// `category = "hook"` ones get dispatched, so those events would
    /// never sound — the inert plugin the manager would paint as a normal
    /// one.
    #[error(
        "`[[contributions.hook]]` requires `category = \"hook\"`: a plugin of another category receives no events"
    )]
    HookOnOtherCategory,
    /// `category = "hook"` with `net`: a hook receives the path of every
    /// mutation on the machine, and with network access that would be a
    /// channel to exfiltrate them. Until an ADR says which badge states
    /// it, it is rejected (ADR 0100).
    #[error(
        "a `hook` cannot declare `net`: it receives the path of every mutation, and with network access that is an exfiltration channel (ADR 0100)"
    )]
    HookWithNet,
    /// `fs-write = "scoped"` (or any string): a RESERVED value no host
    /// gate ever honored and that, since ADR 0088, is rejected instead of
    /// being approved in vain. What exists is
    /// `fs-write = { sidecar = [...] }` (ADR 0101).
    #[error(
        "`fs-write = \"{0}\"` does not exist: a plugin's write is `fs-write = {{ sidecar = [\"name\"] }}`, and only for a `hook` (ADR 0101)"
    )]
    FsWriteReserved(String),
    /// `fs-write = { sidecar = [...] }` on a plugin that is not a `hook`:
    /// only a hook has an event to write alongside, so in another category
    /// it would be an approved capability nobody uses (ADR 0088).
    #[error("`fs-write` with sidecars can only be declared by a `hook` (ADR 0101)")]
    SidecarNotForCategory,
    /// A sidecar name that is not A portable file name: printable ASCII
    /// without `/ \ : * ? " < > |`, not `.`/`..`, not a reserved Windows
    /// name, no trailing dot; or repeated. ASCII on purpose: the name is
    /// what the human reads in the approval badge and what ends up on
    /// disk, and a bidi or invisible character there is a spoof. Carries
    /// the value so it is actionable.
    #[error(
        "invalid sidecar name `{0}`: printable ASCII, no `/ \\ : * ? \" < > |`, not `.`/`..`, not a reserved name, not repeated"
    )]
    SidecarName(String),
    /// `fs-write = {{ sidecar = [...] }}` empty or with more than
    /// [`SIDECAR_MAX_NAMES`] names.
    #[error("`fs-write.sidecar` carries {got} names: between 1 and {SIDECAR_MAX_NAMES}")]
    SidecarListSize {
        /// How many it carried.
        got: usize,
    },
    /// `capabilities.ai` declared when NOTHING honors it: there is no AI
    /// WIT interface nor a place in the host that links it. It used to
    /// parse, go into the digest and paint a badge, so a human would
    /// approve "AI access" and grant nothing — the declared capability
    /// nobody honors (ADR 0088). Rejected while parsing and the field
    /// stays because spec §7.1 names it; hooks had the same rejection
    /// until ADR 0100.
    #[error(
        "the `ai` capability is not implemented yet: there is no WIT interface to serve it, \
         so declaring it would approve a permission that grants nothing"
    )]
    AiNotImplemented,
    /// A `[[contributions.provider]]` claims a scheme it cannot serve: one
    /// from the core ([`CORE_SCHEMES`]), an archive format, or a scheme
    /// with `+` (composition, ADR 0018), or something that is not a
    /// scheme (charset of [`norte_proto::Scheme`]). A plugin serving
    /// `sftp://` would put itself in front of a provider with trash,
    /// resume and TLS, and one serving `ftp://` would receive the saved
    /// FTP passwords; the human approving would see no difference.
    #[error(
        "contributions.provider[].scheme reserved or invalid: `file`, `sftp`, `ftp`, `s3` and \
         archive formats are served by the core, and the scheme must be `[a-z][a-z0-9.-]*`"
    )]
    ReservedScheme,
    /// `plugin.wasm` was compiled against a version of a WIT package that
    /// this host serves as ANOTHER (ADR 0094). Not a manifest error, but
    /// the reason the catalog does not load the plugin, and
    /// [`crate::LoadError`] carries one of these: it is listed as broken
    /// with both versions visible instead of dying inside wasmtime naming
    /// an interface. State (approval) is not touched; a recompiled binary
    /// is another binary and gets approved again (#241).
    #[error(
        "compiled against `{package}@{built_against}`, this norte serves `@{served}`: \
         recompile the plugin against the current WIT"
    )]
    WitMismatch {
        /// The package (`norte:plugin`).
        package: String,
        /// The version the binary references.
        built_against: String,
        /// The one this host serves.
        served: String,
    },
    /// `plugin.wasm` exceeds the artifact cap
    /// ([`crate::MAX_ARTIFACT_BYTES`]) and the catalog does NOT read it:
    /// reading it to hash it and read its imports would materialize in
    /// memory what a third party decided, on every discovery, and a
    /// failure there brings down the whole catalog and not one plugin.
    /// The runtime applies the same cap when instantiating; this is the
    /// same cap, one gate earlier.
    #[error("plugin.wasm is {len} bytes and the cap is {cap}: not read")]
    ArtifactTooLarge {
        /// File size in bytes.
        len: u64,
        /// The cap.
        cap: u64,
    },
    /// Two or more directories declare the SAME `plugin.id` (issue #69):
    /// ALL are rejected (fail-closed). A second directory cannot claim an
    /// approved plugin's id to sneak in its own `plugin.wasm`.
    #[error(
        "duplicate id: `{0}` appears in more than one plugin directory (rejected for security)"
    )]
    DuplicateId(String),
    /// `plugin.description` exceeds the 280-character cap (P1). Cosmetic
    /// but fail-loud, like `id`: prevents manifests from bloating logs/UI
    /// or trying to hide text outside the frontend's truncated view.
    #[error("plugin.description exceeds the 280-character cap")]
    DescriptionTooLong,
    /// `contributions.command[].title` exceeds the 120-character cap (P1
    /// encoding audit M2). Unlike `description`, `title` DOES go into
    /// `approval_digest` (it decides when/how the command fires in the
    /// palette) — the cap is only a PARSING one: an already-approved
    /// manifest with a short title is unaffected if the cap changes in a
    /// future host version, because that only rejects NEW manifests, it
    /// never reinterprets an old one.
    #[error("contributions.command[].title exceeds the 120-character cap")]
    CommandTitleTooLong,
    /// `contributions.command[].id` exceeds the 64-character cap (P1
    /// encoding audit M2). Same criterion as `CommandTitleTooLong`:
    /// parsing cap, does not reinterpret existing approvals.
    #[error("contributions.command[].id exceeds the 64-character cap")]
    CommandIdTooLong,
    /// `[contributions]` declares more than [`COMMAND_MAX_COUNT`]
    /// commands (#281).
    #[error("contributions declares more commands than allowed (cap: {COMMAND_MAX_COUNT})")]
    TooManyCommands,
    /// `[config]` declares more than [`CONFIG_MAX_KEYS`] keys (P2
    /// decision 1).
    #[error("[config] declares more keys than allowed (cap: {CONFIG_MAX_KEYS})")]
    ConfigTooManyKeys,
    /// A `[config.<key>]` key does not respect the `[a-z0-9-]{1,32}`
    /// charset (P2 decision 1). The literal key is NOT interpolated into
    /// the message (same criterion as `Id`): a hostile key must not reach
    /// logs/UI via the error text.
    #[error("invalid [config] key: the `[a-z0-9-]{{1,32}}` charset is expected")]
    ConfigKeyCharset,
    /// `[config.<key>].description` exceeds the
    /// [`CONFIG_DESCRIPTION_MAX_CHARS`] cap (same criterion as
    /// `plugin.description`).
    #[error("[config.<key>].description exceeds the {CONFIG_DESCRIPTION_MAX_CHARS}-character cap")]
    ConfigDescriptionTooLong,
    /// A `string`-typed `[config.<key>].default` exceeds the
    /// [`CONFIG_STRING_MAX_CHARS`] cap (P2 decision 1).
    #[error("[config.<key>].default (string) exceeds the {CONFIG_STRING_MAX_CHARS}-character cap")]
    ConfigDefaultTooLong,
    /// An `int`-typed `[config.<key>].default` falls outside the declared
    /// `[min, max]` (P2 decision 1: defaults MUST validate against their
    /// own type/range while parsing).
    #[error("[config.<key>].default (int) falls outside the declared [min, max] range")]
    ConfigIntDefaultOutOfRange,
    /// `[config.<key>].values` (enum) exceeds [`CONFIG_ENUM_MAX_VALUES`]
    /// entries (P2 decision 1).
    #[error("[config.<key>].values (enum) exceeds the {CONFIG_ENUM_MAX_VALUES}-entry cap")]
    ConfigEnumTooManyValues,
    /// A `[config.<key>].values` (enum) entry exceeds the
    /// [`CONFIG_STRING_MAX_CHARS`] cap (P2 decision 1).
    #[error(
        "[config.<key>].values (enum) contains an entry that exceeds the {CONFIG_STRING_MAX_CHARS}-character cap"
    )]
    ConfigEnumValueTooLong,
    /// An `enum`-typed `[config.<key>].default` is not among `values` (P2
    /// decision 1: defaults MUST validate against their own type/range
    /// while parsing).
    #[error("[config.<key>].default (enum) is not among the declared `values`")]
    ConfigEnumDefaultNotInValues,
    /// The VALUES `config.toml` (P2 decision 3, distinct from the
    /// manifest) fails to validate against the `[config]` schema —
    /// fail-closed at the catalog level: the WHOLE plugin is excluded
    /// (same criterion as `DuplicateId`), it never loads with half-way
    /// values.
    #[error("invalid config.toml: {0}")]
    ConfigValues(#[from] crate::config_values::ConfigValueError),
}

/// Cap on `contributions.command[].title` (P1 encoding audit M2): same
/// spirit as `description`'s cap — a hostile plugin must not be able to
/// bloat the palette with an absurdly long title. `title` DOES go into
/// `approval_digest` (see [`ManifestError::CommandTitleTooLong`]'s doc).
pub const COMMAND_TITLE_MAX_CHARS: usize = 120;

/// Cap on `contributions.command[].id` (P1 encoding audit M2).
pub const COMMAND_ID_MAX_CHARS: usize = 64;

/// Cap on HOW MANY commands a manifest declares (#281), the same size and
/// for the same reason as [`CONFIG_MAX_KEYS`]: every approved command
/// becomes a palette row on every client
/// (`norte_frontend::palette::plugin_rows`), and the place that cuts that
/// off at the root is manifest validation, not each palette.
///
/// Like the other `[[command]]` caps, it is a PARSING cap: it rejects NEW
/// manifests, it never reinterprets an already-granted approval.
pub const COMMAND_MAX_COUNT: usize = 32;

/// The alphabet of a plugin id, defined alongside the wire type that
/// carries it ([`norte_proto::methods::is_valid_plugin_id`]).
///
/// Re-exported under its usual name because it is ONE question with two
/// entry points: this crate asks it while parsing a `plugin.toml`, and
/// everyone who receives a `PluginInfo` over the wire asks it again. Two
/// implementations of the same alphabet would end up with one looser than
/// the other. `norte-core` re-exports it in turn for frontends that do not
/// depend on this crate.
pub use norte_proto::methods::is_valid_plugin_id;

/// Schemes the core serves and that a provider plugin CANNOT claim.
///
/// `ftp` is here, even though its guest is WASM: a plugin claiming it
/// would receive, via `configure`, the saved password of every `ftp://`
/// connection, and the approval screen did not show the scheme. The day
/// the embedded guest ships as a plugin, `ftp` leaves this list in that
/// same commit.
pub const CORE_SCHEMES: &[&str] = &["file", "sftp", "ftp", "s3"];

/// `true` if a `[[contributions.provider]]` can declare `scheme`: it is a
/// valid scheme for a [`norte_proto::VPath`], it is not one of
/// [`CORE_SCHEMES`], it is not an archive format and it carries no `+`,
/// ADR 0018's composition operator (`zip+sftp`).
///
/// ```
/// use norte_plugin_host::scheme_claimable;
/// assert!(scheme_claimable("webdav"));
/// assert!(!scheme_claimable("ftp"));
/// assert!(!scheme_claimable("sftp"));
/// assert!(!scheme_claimable("zip+sftp"));
/// assert!(!scheme_claimable("Web-DAV"));
/// ```
#[must_use]
pub fn scheme_claimable(scheme: &str) -> bool {
    norte_proto::Scheme::new(scheme).is_ok()
        && !scheme.contains('+')
        && !CORE_SCHEMES.contains(&scheme)
        && !norte_proto::ARCHIVE_FORMATS.contains(&scheme)
}

/// What a manifest declares about hooks and sidecars (ADR 0100, ADR 0101),
/// validated apart from [`Manifest::from_toml`] so the manifest's checklist
/// does not overflow the line limit.
fn validate_hooks_and_sidecars(raw: &ManifestRaw) -> Result<(), ManifestError> {
    // Hooks (ADR 0100): every event from the closed vocabulary, and a
    // plugin that declares itself a hook listens to at least one. AFTER
    // the id on purpose: a manifest whose id cannot be trusted is
    // rejected for the id, which is the actionable one.
    if let Some(h) = raw
        .contributions
        .hook
        .iter()
        .find(|h| !HOOK_EVENTS.contains(&h.on.as_str()))
    {
        return Err(ManifestError::HookUnknownEvent(h.on.clone()));
    }
    if raw.plugin.category == Category::Hook && raw.contributions.hook.is_empty() {
        return Err(ManifestError::HookWithoutEvents);
    }
    if raw.plugin.category != Category::Hook && !raw.contributions.hook.is_empty() {
        return Err(ManifestError::HookOnOtherCategory);
    }
    if raw.plugin.category == Category::Hook && raw.capabilities.net.is_some() {
        return Err(ManifestError::HookWithNet);
    }
    // `fs-write` (ADR 0101): only sidecars, only on hooks, real names. An
    // inherited `"scoped"` is rejected along with what to put instead.
    match &raw.capabilities.fs_write {
        crate::capability::FsWriteCap::None => {}
        crate::capability::FsWriteCap::Reserved(s) => {
            return Err(ManifestError::FsWriteReserved(s.clone()));
        }
        crate::capability::FsWriteCap::Sidecar(l) => {
            let sidecar = &l.sidecar;
            if raw.plugin.category != Category::Hook {
                return Err(ManifestError::SidecarNotForCategory);
            }
            if sidecar.is_empty() || sidecar.len() > SIDECAR_MAX_NAMES {
                return Err(ManifestError::SidecarListSize { got: sidecar.len() });
            }
            for (i, n) in sidecar.iter().enumerate() {
                if !is_valid_sidecar_name(n) || sidecar[..i].contains(n) {
                    return Err(ManifestError::SidecarName(n.clone()));
                }
            }
        }
    }
    Ok(())
}

impl Manifest {
    /// Parses and VALIDATES a `plugin.toml`.
    ///
    /// # Errors
    /// [`ManifestError`] if the TOML does not parse, the `id` is not
    /// reverse-DNS, or an `exec` other than `none` is declared (forbidden
    /// with no exception).
    pub fn from_toml(src: &str) -> Result<Self, ManifestError> {
        let mut raw: ManifestRaw = toml::from_str(src)?;
        // `fs-write = "none"` is the explicit form of "no write" that ADR
        // 0022 documents: it is worth the same as absent, and digests the
        // same (byte 0), so no approval moves. Any OTHER string is
        // rejected in the validation below.
        if matches!(&raw.capabilities.fs_write, crate::capability::FsWriteCap::Reserved(s) if s == "none")
        {
            raw.capabilities.fs_write = crate::capability::FsWriteCap::None;
        }
        // Hard invariant: exec is ALWAYS none.
        if raw
            .capabilities
            .exec
            .as_deref()
            .is_some_and(|e| e != "none")
        {
            return Err(ManifestError::ExecForbidden);
        }
        // The root marker is A name, never a path: with a slash inside,
        // the host would be going up a route chosen by the plugin, which
        // is a different capability from the one being approved.
        if let Some(marker) = raw.capabilities.location_root_marker.as_deref() {
            if !raw.capabilities.location.granted() {
                return Err(ManifestError::LocationMarkerWithoutCap);
            }
            let bad = marker.is_empty()
                || marker.len() > 64
                || marker.contains('/')
                || marker.contains('\\')
                || marker.contains('\0')
                || marker == "."
                || marker == "..";
            if bad {
                return Err(ManifestError::LocationMarker);
            }
        }
        // REAL reverse-DNS id: one or more `[A-Za-z0-9-]+` segments
        // separated by dots, with at least one dot, no empty segment (nor
        // leading/trailing dot), total length 1..=128. Hardened beyond
        // "contains a dot" because the manifest's raw id ends up in logs
        // and in the approval modal (T5): an id with newlines, quotes or
        // spaces would allow log injection or spoofing the consent
        // dialog.
        if !is_valid_plugin_id(&raw.plugin.id) {
            return Err(ManifestError::Id);
        }
        validate_hooks_and_sidecars(&raw)?;
        // `ai`: a promise nobody keeps.
        // Presence is checked, not value: any mode would be equally
        // inert.
        if raw.capabilities.ai.is_some() {
            return Err(ManifestError::AiNotImplemented);
        }
        // A provider serves the scheme it declares, so the scheme is a
        // name that can be spoofed: the core's and the archive ones are
        // not given up, and anything that is not a scheme never reaches
        // the connector.
        if raw
            .contributions
            .provider
            .iter()
            .any(|c| !scheme_claimable(&c.scheme))
        {
            return Err(ManifestError::ReservedScheme);
        }
        // 280-CHARACTER cap (not bytes: a non-ASCII language must not pay
        // the cap ahead of time). Cosmetic but fail-loud, like `id`.
        if raw
            .plugin
            .description
            .as_deref()
            .is_some_and(|d| d.chars().count() > 280)
        {
            return Err(ManifestError::DescriptionTooLong);
        }
        // Caps for each declared command (P1 encoding audit M2),
        // symmetric with `description`'s — CHARS, not bytes. `id` first:
        // it is the one that travels over the wire to dispatch
        // (`plugin.run_command`), capping it first gives the more
        // specific error if BOTH overflow at once. How many, before how
        // long each one is: with a thousand commands the useful error is
        // "there are too many", not the long `id` of number 400.
        if raw.contributions.command.len() > COMMAND_MAX_COUNT {
            return Err(ManifestError::TooManyCommands);
        }
        for c in &raw.contributions.command {
            if c.id.chars().count() > COMMAND_ID_MAX_CHARS {
                return Err(ManifestError::CommandIdTooLong);
            }
            if c.title.chars().count() > COMMAND_TITLE_MAX_CHARS {
                return Err(ManifestError::CommandTitleTooLong);
            }
        }
        // `[config]` (P2 decision 1): key count cap first (fail-fast
        // before validating each entry), then charset + per-type caps per
        // key, in `BTreeMap` order (deterministic).
        if raw.config.len() > CONFIG_MAX_KEYS {
            return Err(ManifestError::ConfigTooManyKeys);
        }
        let mut config = BTreeMap::new();
        for (key, entry) in raw.config {
            if !is_valid_config_key(&key) {
                return Err(ManifestError::ConfigKeyCharset);
            }
            config.insert(key, validate_config_entry(entry)?);
        }
        Ok(Self {
            id: raw.plugin.id,
            name: raw.plugin.name,
            publisher: raw.plugin.publisher,
            version: raw.plugin.version,
            category: raw.plugin.category,
            description: raw.plugin.description,
            contributions: raw.contributions,
            capabilities: raw.capabilities,
            config,
        })
    }

    /// Hex (sha256) digest of the manifest's CANONICAL form, to ANCHOR the
    /// human's approval (issue #69, confused-deputy TOCTOU defense).
    /// Covers not just the `[capabilities]` (what the host enforces) but
    /// also `category` and `contributions` — the fields that decide WHEN
    /// and HOW the plugin fires (mimetypes, command ids, schemes…). So a
    /// re-edited `plugin.toml` that changes from `command` to
    /// `previewer`, or that widens the mimetypes, WHILE KEEPING the same
    /// capabilities, stops matching the digest and forces re-consent
    /// (otherwise it would start auto-running in the viewer without the
    /// human approving it for that).
    ///
    /// The form is deterministic and unambiguous (stable enum tags,
    /// length-prefixed strings, network hosts as an ordered, deduplicated
    /// set). The id and name/publisher/version do NOT go in: the approval
    /// is indexed by id (changing it is another plugin) and the rest is
    /// cosmetic — what matters for security is what it does and when it
    /// fires.
    ///
    /// P2 extends the canonical form with a `config:` section — but ONLY
    /// when `[config]` declares some key: `update_config_digest` does not
    /// add a single byte if `self.config` is empty, so a manifest without
    /// `[config]` digests EXACTLY the same as before P2 (existing human
    /// approvals of plugins that do not use `[config]` are not reset).
    #[must_use]
    pub fn approval_digest(&self) -> String {
        use sha2::{Digest, Sha256};
        let mut h = Sha256::new();
        // Domain prefix + schema version: if the canonical form ever
        // changes, old digests won't collide with new ones.
        h.update(b"norte-plugin-manifest:v1\n");
        h.update([self.category.digest_tag()]);
        self.contributions.update_digest(&mut h);
        self.capabilities.update_digest(&mut h);
        update_config_digest(&self.config, &mut h);
        // ADR 0037: OPTIONAL section just like `config:` — see its
        // rustdoc.
        update_decorator_digest(&self.contributions.decorator, &mut h);
        crate::capability::hex_lower(&h.finalize())
    }
}
