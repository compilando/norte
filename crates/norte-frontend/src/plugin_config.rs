//! Editable plugin `[config]` keys (G3c, ADR 0037): a single pure editing
//! widget shared by the extension manager's config section AND the
//! settings overlay's Plugins section (TUI + GUI) — the same
//! bool/enum-cycle + text/int-inline-edit-with-`[min,max]` shape
//! [`crate::settings::SettingsState`] already established for GENERAL
//! settings, but addressed by `(plugin_id, key)` and persisted via
//! `plugin.set_config` (an async RPC through `Backend`) instead of a local
//! `norte.toml` write — different enough persistence that unifying it into
//! `SettingsState` itself would have meant threading two write paths
//! through one state machine; a second small, focused widget stays
//! honest about that difference while still sharing the masking/cycling
//! primitives (see `crate::settings`'s `cycle` helper).
//!
//! `norte_plugin_host::ConfigKeySpec`'s wire twin
//! ([`norte_proto::methods::PluginConfigKeyWire`]) is PLUGIN-declared
//! (`description`) or plugin-adjacent (`key`, already charset-validated by
//! the manifest parser, `[a-z0-9-]{1,32}` — safe to paint as-is) — only
//! `description` needs masking here (same untrusted-text criterion as
//! `PluginInfo::description`).

use norte_proto::methods::PluginConfigKeyWire;

use crate::settings::{SettingsEditError, cycle};

/// A single `[config.<key>]` entry.
///
/// Two halves, and mixing them is the bug this shape exists to prevent:
///
/// - `key`, `kind`, `default`, `min`, `max`, `values` and `value` are the
///   OPERANDS. `value` is what [`PluginConfigState::activate`] cycles and
///   what ends up in [`PendingConfigWrite`], so it must stay exactly as it
///   came off the wire — masking it in place would write the mask into the
///   plugin's config.
/// - [`Self::display`] is the same three free-text fields, ALREADY masked,
///   and is the only half a frontend may paint.
///
/// The previous doc here claimed `default`/`values`/`value` were "norte's OWN
/// vocabulary or numbers, never free plugin text". That was wrong:
/// `norte-plugin-host`'s manifest validation bounds only their LENGTH
/// (`CONFIG_STRING_MAX_CHARS`, `CONFIG_ENUM_MAX_VALUES`) and checks no
/// charset, so a `plugin.toml` could put U+202E in an enum value and have it
/// reach a DOM text node untouched. `key` and `kind` really are constrained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigKeyRow {
    /// `[config.<key>]`'s key.
    pub key: String,
    /// `"string"`, `"bool"`, `"int"` or `"enum"` — a value outside this
    /// closed set (a newer wire) is treated as read-only TEXT by
    /// [`PluginConfigState::activate`] (forward-compat: never a panic).
    pub kind: String,
    /// Schema default, as display text.
    pub default: String,
    /// Inclusive lower bound (`kind == "int"` only).
    pub min: Option<i64>,
    /// Inclusive upper bound (`kind == "int"` only).
    pub max: Option<i64>,
    /// Allowed values (`kind == "enum"` only); empty otherwise.
    pub values: Vec<String>,
    /// Cosmetic description, ALREADY masked (plugin text — untrusted).
    pub description: String,
    /// Current effective value: the schema default overlaid with whatever the
    /// user's `config.toml` says. THE OPERAND, raw off the wire. To paint it,
    /// use [`Self::display`].
    pub value: String,
    /// `value`, `default` and `values`, masked for painting.
    pub display: ConfigKeyDisplay,
}

/// The `kind`s this build knows how to edit — the SAME closed set
/// [`PluginConfigState::activate`] dispatches on.
///
/// It lives here so a frontend that paints "read-only" and the function that
/// decides read-only cannot drift: a screen that greys out a row `activate`
/// would happily cycle tells its reader the write failed.
pub const EDITABLE_KINDS: &[&str] = &["bool", "enum", "string", "int"];

impl ConfigKeyRow {
    /// `true` if this build knows how to edit this key's `kind`.
    ///
    /// ```
    /// use norte_frontend::plugin_config::sanitize_config_keys;
    /// use norte_proto::methods::PluginConfigKeyWire;
    ///
    /// let rows = sanitize_config_keys(&[PluginConfigKeyWire {
    ///     key: "future".to_owned(),
    ///     kind: "duration".to_owned(),
    ///     default: "1s".to_owned(),
    ///     min: None,
    ///     max: None,
    ///     values: Vec::new(),
    ///     description: None,
    ///     value: "1s".to_owned(),
    /// }]);
    /// // A kind from a newer peer: read-only, never a panic.
    /// assert!(!rows[0].is_editable());
    /// ```
    #[must_use]
    pub fn is_editable(&self) -> bool {
        EDITABLE_KINDS.contains(&self.kind.as_str())
    }
}

/// The free-text halves of a [`ConfigKeyRow`], masked and ready to paint.
///
/// Separate from the operands on purpose: a frontend that reaches for
/// `row.value` to paint it gets the raw bytes and a reviewer sees it; one
/// that reaches for `row.display.value` cannot accidentally write it back.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConfigKeyDisplay {
    /// The effective value, masked.
    pub value: String,
    /// The schema default, masked.
    pub default: String,
    /// The allowed values of an `enum`, each masked.
    pub values: Vec<String>,
    /// At least one of the three paints DIFFERENTLY from what it is. The
    /// frontend marks it; it never hides it.
    pub hostile: bool,
}

/// Sanitizes a `plugin.get_config` result into rows that carry both halves:
/// the OPERANDS raw off the wire, and a [`ConfigKeyDisplay`] with every
/// free-text field masked through [`crate::display_name`].
///
/// Four fields are plugin-authored free text, not one: `description`,
/// `default`, each entry of `values`, and `value` — which is the schema
/// default overlaid with the user's `config.toml`, so the project config
/// layer feeds it too. Only `description` was masked before, and the other
/// three reached the DOM untouched.
///
/// `key` and `kind` pass through because they really are constrained: `key`
/// is charset-validated by the manifest parser and `kind` is a closed set.
#[must_use]
pub fn sanitize_config_keys(keys: &[PluginConfigKeyWire]) -> Vec<ConfigKeyRow> {
    keys.iter()
        .map(|k| {
            let (value, v_hostile) = crate::display_name(k.value.as_bytes());
            let (default, d_hostile) = crate::display_name(k.default.as_bytes());
            let dominio: Vec<(String, bool)> = k
                .values
                .iter()
                .map(|v| crate::display_name(v.as_bytes()))
                .collect();
            let hostile = v_hostile || d_hostile || dominio.iter().any(|(_, h)| *h);
            ConfigKeyRow {
                key: k.key.clone(),
                kind: k.kind.clone(),
                default: k.default.clone(),
                min: k.min,
                max: k.max,
                values: k.values.clone(),
                description: k
                    .description
                    .as_deref()
                    .map(|d| crate::display_name(d.as_bytes()).0)
                    .unwrap_or_default(),
                value: k.value.clone(),
                display: ConfigKeyDisplay {
                    value,
                    default,
                    values: dominio.into_iter().map(|(v, _)| v).collect(),
                    hostile,
                },
            }
        })
        .collect()
}

/// A value ready to persist via `Backend::plugin_set_config(plugin_id, &key,
/// &value)` — `plugin_id` is NOT carried here (the caller already knows
/// which plugin's [`PluginConfigState`] produced this; threading it through
/// every write would duplicate what the caller already has).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConfigWrite {
    /// `[config.<key>]`'s key.
    pub key: String,
    /// New value, canonically encoded (same encoding as
    /// [`ConfigKeyRow::value`] — `bool` → `"true"`/`"false"`, `int` →
    /// decimal).
    pub value: String,
    /// New value as display text (identical to `value` for every kind
    /// today — kept separate for symmetry with
    /// [`crate::settings::PendingWrite::display`], in case a future kind
    /// needs a display form that differs from the wire form).
    pub display: String,
}

/// Pure editor over ONE plugin's `[config]` keys: cursor over
/// [`ConfigKeyRow`]s, `bool`/`enum` CYCLE immediately on
/// [`Self::activate`], `string`/`int` open an inline edit buffer
/// ([`Self::edit_commit`] confirms). No filter/search (a plugin has at
/// most `CONFIG_MAX_KEYS` = 32 keys — `norte_plugin_host::CONFIG_MAX_KEYS`
/// — small enough that browsing beats filtering). Mirrors
/// [`crate::settings::SettingsState`]'s `activate`/`edit_commit` shape;
/// see the module doc for why it's a separate type.
#[derive(Debug, Clone)]
pub struct PluginConfigState {
    rows: Vec<ConfigKeyRow>,
    cursor: usize,
    edit: Option<String>,
}

impl PluginConfigState {
    /// Opens the editor over `rows` (a [`sanitize_config_keys`] snapshot).
    #[must_use]
    pub fn new(rows: Vec<ConfigKeyRow>) -> Self {
        Self {
            rows,
            cursor: 0,
            edit: None,
        }
    }

    /// The rows, in the order [`sanitize_config_keys`] produced them (wire
    /// order — `PLUGIN_GET_CONFIG`'s rustdoc: manifest key order).
    #[must_use]
    pub fn rows(&self) -> &[ConfigKeyRow] {
        &self.rows
    }

    /// Selection position within [`Self::rows`].
    #[must_use]
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// Moves the selection up (clamped at the top). No-op while editing.
    pub fn up(&mut self) {
        if self.edit.is_none() {
            self.cursor = self.cursor.saturating_sub(1);
        }
    }

    /// Moves the selection down (clamped at the end). No-op while editing.
    pub fn down(&mut self) {
        if self.edit.is_none() && self.cursor + 1 < self.rows.len() {
            self.cursor += 1;
        }
    }

    /// `true` while the inline edit buffer (`string`/`int`) is active.
    #[must_use]
    pub fn is_editing(&self) -> bool {
        self.edit.is_some()
    }

    /// The RAW edit buffer, for painting (sanitizing happens on paint, same
    /// contract as [`crate::settings::SettingsState::edit_buffer`]).
    #[must_use]
    pub fn edit_buffer(&self) -> Option<&str> {
        self.edit.as_deref()
    }

    /// Appends a char to the edit buffer. No-op if not editing.
    pub fn edit_push_char(&mut self, c: char) {
        if let Some(buf) = &mut self.edit {
            buf.push(c);
        }
    }

    /// Removes the last char from the edit buffer. No-op if not editing.
    pub fn edit_backspace(&mut self) {
        if let Some(buf) = &mut self.edit {
            buf.pop();
        }
    }

    /// Cancels the edit WITHOUT writing — the row's value stays as it was.
    pub fn edit_cancel(&mut self) {
        self.edit = None;
    }

    /// Enter over the row under the cursor: `bool`/`enum` CYCLE immediately
    /// (return the [`PendingConfigWrite`] right away); `string`/`int` OPEN
    /// the edit buffer (return `None` — [`Self::edit_commit`] produces the
    /// write once confirmed). An unknown `kind` (forward-compat, a newer
    /// wire) is treated as read-only: always `None`, never opens an edit
    /// buffer for a shape this build doesn't understand. Nothing selected
    /// (empty rows) also returns `None`.
    pub fn activate(&mut self) -> Option<PendingConfigWrite> {
        let row = self.rows.get(self.cursor)?;
        match row.kind.as_str() {
            // `EDITABLE_KINDS` is the same set, one screen up. If you add an
            // arm here, add it there: a frontend that greys out a row this
            // function DOES edit tells its reader the write failed.
            "bool" => {
                let next = row.value != "true";
                Some(self.commit_row(next.to_string()))
            }
            "enum" => {
                let refs: Vec<&str> = row.values.iter().map(String::as_str).collect();
                let next = cycle(&row.value, &refs);
                Some(self.commit_row(next))
            }
            "string" | "int" => {
                self.edit = Some(row.value.clone());
                None
            }
            // Unknown kind: read-only (forward-compat, ADR 0037 vocabulary
            // is closed today but a newer peer could add one).
            _ => None,
        }
    }

    /// Confirms the inline edit buffer: `int` parses as `i64` and validates
    /// `[min, max]` CLIENT-SIDE (the daemon re-validates server-side too —
    /// spec S2 "validated client-side AND server-side", defense in depth,
    /// never a race the UI can win by skipping its own check); `string`
    /// accepts anything. Only reachable with [`Self::is_editing`] — the
    /// caller guarantees it; without an active edit this returns
    /// [`SettingsEditError::NotAnInt`] as an inert fallback (unreachable in
    /// practice, same defensive shape as
    /// [`crate::settings::SettingsState::edit_commit`]).
    ///
    /// # Errors
    /// [`SettingsEditError::NotAnInt`] if an `int` row's buffer does not
    /// parse as a whole number (or there is no active edit);
    /// [`SettingsEditError::OutOfRange`] if it parses but falls outside
    /// `[min, max]` (an absent bound is unbounded on that side). Never for
    /// a `string` row.
    pub fn edit_commit(&mut self) -> Result<PendingConfigWrite, SettingsEditError> {
        let Some(buf) = self.edit.clone() else {
            return Err(SettingsEditError::NotAnInt);
        };
        let Some(row) = self.rows.get(self.cursor) else {
            return Err(SettingsEditError::NotAnInt);
        };
        let write = if row.kind == "int" {
            let n: i64 = buf
                .trim()
                .parse()
                .map_err(|_| SettingsEditError::NotAnInt)?;
            let min = row.min.unwrap_or(i64::MIN);
            let max = row.max.unwrap_or(i64::MAX);
            if n < min || n > max {
                return Err(SettingsEditError::OutOfRange {
                    min: row.min.unwrap_or(n),
                    max: row.max.unwrap_or(n),
                });
            }
            self.commit_row(n.to_string())
        } else {
            self.commit_row(buf.clone())
        };
        self.edit = None;
        Ok(write)
    }

    /// OPTIMISTIC update of the row under the cursor to `value` + builds
    /// its [`PendingConfigWrite`] — same "optimistic now, corrected by the
    /// next fetch if the write failed" contract as
    /// [`crate::settings::SettingsState::commit_row`].
    fn commit_row(&mut self, value: String) -> PendingConfigWrite {
        let row = &mut self.rows[self.cursor];
        row.value.clone_from(&value);
        // And the PAINTED half too. They are two halves of one row and only
        // the operand was being updated, so the cell kept showing the old
        // value: cycling a `bool` wrote `false`, painted `true`, and the next
        // Enter wrote `true` again — the daemon flip-flopped and the screen
        // never moved. The new value can be plugin text (an `enum` value) or
        // human-typed, so it goes through the same mask as the rest.
        let (pintable, hostile) = crate::display_name(value.as_bytes());
        row.display.value.clone_from(&pintable);
        // The row's flag is about ALL THREE free-text fields, so it can only
        // grow here: a clean new value does not clear a hostile `default` or
        // a hostile domain.
        row.display.hostile |= hostile;
        PendingConfigWrite {
            key: row.key.clone(),
            display: pintable,
            value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cycling a `bool` moves BOTH halves of the row.
    ///
    /// Only the operand used to update, so the cell kept showing the old
    /// value: the second Enter sent it back to where it was, the daemon
    /// oscillated, and the screen never moved.
    #[test]
    fn cycling_also_moves_what_is_painted() {
        let rows = sanitize_config_keys(&[PluginConfigKeyWire {
            key: "verbose".to_owned(),
            kind: "bool".to_owned(),
            default: "false".to_owned(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: "false".to_owned(),
        }]);
        let mut state = PluginConfigState::new(rows);
        state.activate().expect("a bool cycles");
        assert_eq!(state.rows()[0].value, "true", "the operand");
        assert_eq!(state.rows()[0].display.value, "true", "and what is painted");
    }

    /// `EDITABLE_KINDS` and `activate` are the same set, and this is what
    /// makes "they must not drift" more than a comment: a `kind` the flag
    /// calls editable that `activate` refuses (or the other way round) is a
    /// screen that lies about what it can do.
    #[test]
    fn the_editable_set_is_the_one_activate_dispatches() {
        for kind in ["bool", "enum", "string", "int", "duration", ""] {
            let wire = PluginConfigKeyWire {
                key: "k".to_owned(),
                kind: kind.to_owned(),
                default: "a".to_owned(),
                min: None,
                max: None,
                values: vec!["a".to_owned(), "b".to_owned()],
                description: None,
                value: "a".to_owned(),
            };
            let rows = sanitize_config_keys(std::slice::from_ref(&wire));
            let editable = rows[0].is_editable();
            let mut state = PluginConfigState::new(rows);
            // `activate` does SOMETHING — writes, or opens the buffer —
            // exactly for the `kind`s the flag calls editable.
            let did_something = state.activate().is_some() || state.is_editing();
            assert_eq!(editable, did_something, "kind `{kind}`");
        }
    }

    fn wire(key: &str, kind: &str, default: &str, value: &str) -> PluginConfigKeyWire {
        PluginConfigKeyWire {
            key: key.into(),
            kind: kind.into(),
            default: default.into(),
            min: None,
            max: None,
            values: Vec::new(),
            description: None,
            value: value.into(),
        }
    }

    #[test]
    fn sanitize_config_keys_masks_only_description() {
        let hostile = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("corpus fixture");
        let desc = String::from_utf8_lossy(&hostile.bytes).into_owned();
        let mut k = wire("greeting", "string", "hola", "hola");
        k.description = Some(desc);
        let rows = sanitize_config_keys(&[k]);
        assert_eq!(rows[0].key, "greeting");
        assert!(
            !rows[0]
                .description
                .chars()
                .any(norte_encoding::is_terminal_hazard),
            "hostile description left unmasked: {:?}",
            rows[0].description
        );
    }

    #[test]
    fn activate_on_bool_cycles_immediately() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "verbose", "bool", "false", "false",
        )]));
        let w = s.activate().expect("bool activates immediately");
        assert_eq!(w.key, "verbose");
        assert_eq!(w.value, "true");
        assert_eq!(s.rows()[0].value, "true", "optimistic");
        assert!(!s.is_editing());
    }

    #[test]
    fn activate_on_enum_cycles_with_wrap() {
        let mut k = wire("mode", "enum", "fast", "fast");
        k.values = vec!["fast".into(), "thorough".into()];
        let mut s = PluginConfigState::new(sanitize_config_keys(&[k]));
        let w1 = s.activate().unwrap();
        assert_eq!(w1.value, "thorough");
        let w2 = s.activate().unwrap();
        assert_eq!(w2.value, "fast", "wrap");
    }

    #[test]
    fn activate_on_string_opens_editing_without_persisting() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "greeting", "string", "hola", "hola",
        )]));
        let w = s.activate();
        assert!(
            w.is_none(),
            "string does not persist on opening: it only edits"
        );
        assert!(s.is_editing());
        assert_eq!(s.edit_buffer(), Some("hola"));
    }

    #[test]
    fn edit_commit_on_string_persists_what_was_typed() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "greeting", "string", "hola", "hola",
        )]));
        s.activate();
        s.edit_backspace();
        s.edit_backspace();
        s.edit_backspace();
        s.edit_backspace();
        for c in "hey".chars() {
            s.edit_push_char(c);
        }
        let w = s.edit_commit().expect("string is always valid");
        assert_eq!(w.key, "greeting");
        assert_eq!(w.value, "hey");
        assert!(!s.is_editing());
        assert_eq!(s.rows()[0].value, "hey");
    }

    #[test]
    fn edit_commit_on_int_validates_range_without_persisting_and_keeps_the_buffer() {
        let mut k = wire("retries", "int", "3", "3");
        k.min = Some(0);
        k.max = Some(10);
        let mut s = PluginConfigState::new(sanitize_config_keys(&[k]));
        s.activate();
        s.edit_backspace();
        for c in "99".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("99 is out of [0,10]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 0, max: 10 });
        assert!(s.is_editing(), "the buffer is kept after a rejection");
        assert_eq!(s.edit_buffer(), Some("99"));
    }

    #[test]
    fn edit_commit_on_int_rejects_a_non_numeric_value() {
        let mut k = wire("retries", "int", "3", "3");
        k.min = Some(0);
        k.max = Some(10);
        let mut s = PluginConfigState::new(sanitize_config_keys(&[k]));
        s.activate();
        for c in "abc".chars() {
            s.edit_push_char(c);
        }
        assert_eq!(s.edit_commit().unwrap_err(), SettingsEditError::NotAnInt);
    }

    #[test]
    fn edit_commit_on_int_with_no_bounds_accepts_any_integer() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire("count", "int", "0", "0")]));
        s.activate();
        s.edit_backspace();
        for c in "-1000000".chars() {
            s.edit_push_char(c);
        }
        let w = s.edit_commit().expect("with no min/max, any i64 is valid");
        assert_eq!(w.value, "-1000000");
    }

    #[test]
    fn edit_cancel_does_not_persist_and_keeps_the_original_value() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "greeting", "string", "hola", "hola",
        )]));
        s.activate();
        s.edit_push_char('x');
        s.edit_cancel();
        assert!(!s.is_editing());
        assert_eq!(s.rows()[0].value, "hola");
    }

    #[test]
    fn activate_on_an_unknown_kind_is_a_no_op_for_forward_compat() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "future", "duration", "1s", "1s",
        )]));
        assert!(s.activate().is_none());
        assert!(!s.is_editing());
    }

    #[test]
    fn up_down_clamp_and_are_a_no_op_when_empty() {
        let mut s = PluginConfigState::new(Vec::new());
        s.up();
        s.down();
        assert_eq!(s.cursor(), 0);
        assert!(s.activate().is_none());
    }
}
