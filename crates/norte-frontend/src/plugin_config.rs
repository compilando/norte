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

/// A single `[config.<key>]` entry, sanitized and ready to paint:
/// `description` already masked (plugin text, untrusted); everything else
/// (`key`, `kind`, `default`, `min`, `max`, `values`, `value`) comes
/// straight off the wire — `key` is charset-safe (manifest-validated),
/// `kind`/`values`/numeric bounds are norte's OWN vocabulary or numbers,
/// never free plugin text.
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
    /// Current effective value, as display text.
    pub value: String,
}

/// Sanitizes a `plugin.get_config` result into display-ready rows: masks
/// EVERY `description` ([`crate::display_name`], same criterion as
/// `PluginInfo::description`/`DecorationWire::badge`) — the only field a
/// plugin's manifest can fill with arbitrary hostile text; `key`/`kind`/
/// `default`/`min`/`max`/`values`/`value` pass through (charset-safe or
/// norte's own vocabulary, see the [`ConfigKeyRow`] doc).
#[must_use]
pub fn sanitize_config_keys(keys: &[PluginConfigKeyWire]) -> Vec<ConfigKeyRow> {
    keys.iter()
        .map(|k| ConfigKeyRow {
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
        PendingConfigWrite {
            key: row.key.clone(),
            display: value.clone(),
            value,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn sanitize_config_keys_enmascara_solo_description() {
        let hostil = norte_testkit::corpus::hostile_names()
            .into_iter()
            .find(|n| n.id == "rtl_override")
            .expect("fixture del corpus");
        let desc = String::from_utf8_lossy(&hostil.bytes).into_owned();
        let mut k = wire("greeting", "string", "hola", "hola");
        k.description = Some(desc);
        let rows = sanitize_config_keys(&[k]);
        assert_eq!(rows[0].key, "greeting");
        assert!(
            !rows[0]
                .description
                .chars()
                .any(norte_encoding::is_terminal_hazard),
            "description hostil sin enmascarar: {:?}",
            rows[0].description
        );
    }

    #[test]
    fn activate_en_bool_cicla_de_inmediato() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "verbose", "bool", "false", "false",
        )]));
        let w = s.activate().expect("bool activa de inmediato");
        assert_eq!(w.key, "verbose");
        assert_eq!(w.value, "true");
        assert_eq!(s.rows()[0].value, "true", "optimista");
        assert!(!s.is_editing());
    }

    #[test]
    fn activate_en_enum_cicla_con_wrap() {
        let mut k = wire("mode", "enum", "fast", "fast");
        k.values = vec!["fast".into(), "thorough".into()];
        let mut s = PluginConfigState::new(sanitize_config_keys(&[k]));
        let w1 = s.activate().unwrap();
        assert_eq!(w1.value, "thorough");
        let w2 = s.activate().unwrap();
        assert_eq!(w2.value, "fast", "wrap");
    }

    #[test]
    fn activate_en_string_abre_edicion_sin_persistir() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "greeting", "string", "hola", "hola",
        )]));
        let w = s.activate();
        assert!(w.is_none(), "string no persiste al abrir: solo edita");
        assert!(s.is_editing());
        assert_eq!(s.edit_buffer(), Some("hola"));
    }

    #[test]
    fn edit_commit_en_string_persiste_lo_tecleado() {
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
        let w = s.edit_commit().expect("string siempre válido");
        assert_eq!(w.key, "greeting");
        assert_eq!(w.value, "hey");
        assert!(!s.is_editing());
        assert_eq!(s.rows()[0].value, "hey");
    }

    #[test]
    fn edit_commit_en_int_valida_rango_sin_persistir_y_conserva_el_buffer() {
        let mut k = wire("retries", "int", "3", "3");
        k.min = Some(0);
        k.max = Some(10);
        let mut s = PluginConfigState::new(sanitize_config_keys(&[k]));
        s.activate();
        s.edit_backspace();
        for c in "99".chars() {
            s.edit_push_char(c);
        }
        let err = s.edit_commit().expect_err("99 fuera de [0,10]");
        assert_eq!(err, SettingsEditError::OutOfRange { min: 0, max: 10 });
        assert!(s.is_editing(), "el buffer se conserva tras un rechazo");
        assert_eq!(s.edit_buffer(), Some("99"));
    }

    #[test]
    fn edit_commit_en_int_no_numerico_rechaza() {
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
    fn edit_commit_en_int_sin_cotas_acepta_cualquier_entero() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire("count", "int", "0", "0")]));
        s.activate();
        s.edit_backspace();
        for c in "-1000000".chars() {
            s.edit_push_char(c);
        }
        let w = s.edit_commit().expect("sin min/max, cualquier i64 vale");
        assert_eq!(w.value, "-1000000");
    }

    #[test]
    fn edit_cancel_no_persiste_y_conserva_el_valor_original() {
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
    fn activate_en_kind_desconocido_es_no_op_forward_compat() {
        let mut s = PluginConfigState::new(sanitize_config_keys(&[wire(
            "future", "duration", "1s", "1s",
        )]));
        assert!(s.activate().is_none());
        assert!(!s.is_editing());
    }

    #[test]
    fn up_down_clampan_y_son_no_op_vacio() {
        let mut s = PluginConfigState::new(Vec::new());
        s.up();
        s.down();
        assert_eq!(s.cursor(), 0);
        assert!(s.activate().is_none());
    }
}
