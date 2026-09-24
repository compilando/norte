//! The extension manager (`app.extensions`) as seen from the host: what is
//! installed, in what state, and what has been configured on it.
//!
//! **It already governs, with the brake on.** Approving a plugin's
//! capabilities is the extension system's security decision: it is what
//! separates "this code is on your disk" from "this code can read your
//! files". Since 6.4 this window makes that call, with three things that are
//! not decoration:
//!
//! - **Approve ASKS**, and the question lists the capabilities one per
//!   line. Revoke and disable do not ask: they go in the safe direction.
//! - **None of this exists in `SoloRead`.** The same switch that decides
//!   whether the window deletes decides whether it grants permissions.
//! - **The truth lives in the core.** After a change the catalogue is
//!   RE-REQUESTED instead of touching the `bool` here: a local optimism the
//!   daemon has not confirmed is a screen lying about who can read your
//!   files.
//!
//! Everything a plugin writes — its name, its publisher, its version, its
//! description, each key's description, each key's VALUE, its default and
//! an `enum`'s values — is THIRD-PARTY text and is masked at the ENTRY
//! point, which is this module. The only things that are not are the KEY
//! (charset validated by the manifest) and the TYPE (a closed set); the
//! bounds are numbers. That list used to say the value, default and domain
//! were safe: they are not — the manifest only bounds their length, nothing
//! else.

use norte_frontend::help_badge::{plugin_description, plugin_label, plugin_label_flagged};
use norte_frontend::plugin_config::{PendingConfigWrite, PluginConfigState, sanitize_config_keys};
use norte_frontend::settings::SettingsEditError;
use norte_i18n::Lang;
use norte_proto::methods::{PluginGetConfigResult, PluginInfo, PluginListResult};

use crate::bridge::clamp_display;
use crate::dto::{
    ExtensionCommandView, ExtensionConfigRowView, ExtensionDetailView, ExtensionErrorView,
    ExtensionRowView, ExtensionsView,
};

/// Cap on catalogue rows that cross over.
///
/// A hostile daemon can announce as many plugins as it likes, and each row
/// costs several masked strings. The cap bounds the work and the message;
/// what is left out is NOT kept silent, it is stated in the view itself.
pub(crate) const MAX_EXTENSIONS: usize = 512;

/// The open manager.
pub(crate) struct Extensions {
    /// What is installed, already sanitized. Empty while the catalogue has
    /// not arrived.
    rows: Vec<ExtensionRowView>,
    /// The directories that failed to load, already sanitized.
    errors: Vec<ExtensionErrorView>,
    /// Which one is selected.
    cursor: usize,
    /// The catalogue has not answered yet.
    loading: bool,
    /// The open detail card, if any.
    detail: Option<Detail>,
    /// The RAW catalogue as it arrived, already filtered and capped.
    ///
    /// The rows above are its masked projection, and there is no going back
    /// from a mask: asking "do you approve THESE capabilities?" requires
    /// masking each one SEPARATELY and knowing which one differs, and that
    /// can only be done from the original text.
    catalog: Vec<PluginInfo>,
    /// Each extension's commands, already masked, by extension id.
    ///
    /// Kept apart from the row, not inside it: the row crosses the bridge on
    /// EVERY catalogue repaint, and the commands' titles are only needed
    /// when someone opens a detail card. The masking is done once, here.
    commands: std::collections::HashMap<String, Vec<ExtensionCommandView>>,
    /// The extension whose detail card has been REQUESTED. Kept to discard a
    /// response that arrives after the reader has already moved to another
    /// row: without this, a slow detail card would land on top of a
    /// different extension.
    requested: Option<String>,
}

impl Extensions {
    /// Opens the empty manager: the catalogue is requested and arrives
    /// later.
    pub(crate) fn open() -> Self {
        Self {
            rows: Vec::new(),
            errors: Vec::new(),
            cursor: 0,
            loading: true,
            detail: None,
            catalog: Vec::new(),
            commands: std::collections::HashMap::new(),
            requested: None,
        }
    }

    /// Feeds in the catalogue the daemon answered with.
    pub(crate) fn set_catalog(&mut self, list: &PluginListResult) {
        self.loading = false;
        // Who was selected, by ID. A state change RE-REQUESTS the whole
        // catalogue, and the core sorts it by category and id: approving
        // an extension can move it elsewhere, and a cursor by position
        // would leave the reader pointing at a different one right after
        // granting permissions to the first.
        let selected = self.chosen().map(str::to_owned);
        // And a BROKEN one, by what is painted for it: it stays selected
        // even if the catalogue above changes size. By position, a
        // catalogue with one fewer loaded entry left the cursor on a
        // different row, and the next `e` enabled an extension nobody chose.
        let broken_selected = self.broken_chosen().map(|r| (r.dir.clone(), r.hostile));
        self.catalog = list
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(MAX_EXTENSIONS)
            .cloned()
            .collect();
        self.commands = list
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(MAX_EXTENSIONS)
            .map(|p| (p.id.clone(), commands_of(p)))
            .collect();
        self.rows = list
            .plugins
            .iter()
            .filter(|p| norte_proto::methods::is_valid_plugin_id(&p.id))
            .take(MAX_EXTENSIONS)
            .map(row_from)
            .collect();
        self.errors = list
            .errors
            .iter()
            .take(MAX_EXTENSIONS)
            .map(|e| {
                // The BYTES if the peer sends them (#265), and only then can
                // `display_name` do the conversion and FLAG it. The `dir`
                // string is the fallback for a 0.52 peer, which is where the
                // heuristic below still applies.
                let (dir, masked) = norte_frontend::display_name(
                    e.dir_bytes.as_deref().unwrap_or(e.dir.as_bytes()),
                );
                let (reason, reason_masked) = norte_frontend::display_name(e.reason.as_bytes());
                ExtensionErrorView {
                    dir: clamp_display(dir),
                    // Or the U+FFFD was already there. The daemon sends `dir`
                    // as a `String` and produces it with an UNFLAGGED
                    // `to_string_lossy`, so a directory named `caf\xff`
                    // arrives here already converted: `display_name` does
                    // not flag it again — U+FFFD is not a terminal hazard, it
                    // is Specials — and the row claimed to be faithful. See
                    // the double-lossy trap: the flag is not recovered, but
                    // the REPLACEMENT character IS visible, and seeing it
                    // already means what is painted differs from what is
                    // there.
                    //
                    // It used to be a half-solution: `lossy_collapse_ff` and
                    // `lossy_collapse_fe` collapsed into the same row, and
                    // telling them apart needed the bytes. Since 0.53.0 the
                    // daemon sends them (#265) and this branch is only the
                    // fallback for an old peer.
                    // With bytes, `display_name`'s flag is the trustworthy
                    // one and the heuristic is redundant — and would be a
                    // false positive on a directory truly named
                    // `caf\u{FFFD}`. Without them, it remains the only thing
                    // there is.
                    hostile: masked || (e.dir_bytes.is_none() && already_lossy_converted(&e.dir)),
                    // The core writes the reason, but it can QUOTE the
                    // plugin's manifest — and a `Path::display()` — so it
                    // enters through the same door as the rest of the
                    // third-party text.
                    reason: clamp_display(reason),
                    reason_hostile: reason_masked || already_lossy_converted(&e.reason),
                    id: norte_frontend::broken_plugin::uninstallable_id(e, &list.plugins),
                }
            })
            .collect();
        if let Some(i) = selected.and_then(|id| self.rows.iter().position(|f| f.id == id)) {
            self.cursor = i;
        } else if let Some(j) = broken_selected.and_then(|(dir, hostile)| {
            self.errors
                .iter()
                .position(|e| e.dir == dir && e.hostile == hostile)
        }) {
            self.cursor = self.rows.len() + j;
        } else {
            // The one that was selected is gone — or the catalogue arrived
            // empty, which is what happens when the request times out: the
            // cursor falls wherever it can and the DETAIL CARD closes.
            // Without this, the detail kept describing one extension while
            // the cursor pointed at another, and the next key press applied
            // to the one pointed at.
            self.cursor = self.cursor.min(self.total().saturating_sub(1));
            self.close_detail();
        }
    }

    /// The whole selected row: what is needed to govern it.
    pub(crate) fn row_chosen(&self) -> Option<&ExtensionRowView> {
        self.rows.get(self.cursor)
    }

    /// Rows the cursor walks: the loaded ones and, behind them, the ones
    /// that failed.
    fn total(&self) -> usize {
        self.rows.len() + self.errors.len()
    }

    /// The one that did NOT load under the cursor, if the cursor is on one.
    ///
    /// They go behind the loaded ones, in the order they traveled: row
    /// `rows.len() + j` is `errors[j]`. A cursor that only walked the
    /// catalogue left a broken extension with no way to ask for its removal.
    pub(crate) fn broken_chosen(&self) -> Option<&ExtensionErrorView> {
        self.cursor
            .checked_sub(self.rows.len())
            .and_then(|j| self.errors.get(j))
    }

    /// Which row is pointed to. Counts the loaded ones and, behind them, the
    /// broken ones.
    pub(crate) fn cursor(&self) -> usize {
        self.cursor
    }

    /// The one that failed to load and is uninstalled with this id, if any.
    pub(crate) fn broken(&self, id: &str) -> Option<&ExtensionErrorView> {
        self.errors.iter().find(|e| e.id.as_deref() == Some(id))
    }

    /// The id of row `row` — loaded or broken — as it traveled. A broken
    /// one with no id names nothing.
    pub(crate) fn row_id(&self, row: usize) -> Option<&str> {
        match row.checked_sub(self.rows.len()) {
            None => self.rows.get(row).map(|f| f.id.as_str()),
            Some(j) => self.errors.get(j).and_then(|e| e.id.as_deref()),
        }
    }

    /// The raw catalogue, to hand to help: its extension pages come from the
    /// same list as these rows.
    pub(crate) fn catalog(&self) -> &[PluginInfo] {
        &self.catalog
    }

    /// What must be SHOWN before granting capabilities: the extension's
    /// name and its capabilities, each masked on its own and with its own
    /// flag.
    ///
    /// One flag for the whole block does not work here: the reader needs to
    /// know WHICH of the lines paints differently from what it says, and
    /// that line is exactly the one a hostile manifest writes to look like
    /// another capability.
    pub(crate) fn concesion(&self, id: &str) -> Option<Grant> {
        let p = self.catalog.iter().find(|p| p.id == id)?;
        Some(Grant {
            name: third_party_text(&p.name),
            capabilities: p.capabilities.iter().map(|c| third_party_text(c)).collect(),
            digest: p.manifest_digest.clone(),
        })
    }

    /// `true` if the open detail card belongs to this extension.
    pub(crate) fn is_tab_of(&self, id: &str) -> bool {
        self.detail.as_ref().is_some_and(|f| f.id == id)
    }

    /// An extension's commands, already masked.
    pub(crate) fn id_commands(&self, id: &str) -> &[ExtensionCommandView] {
        self.commands.get(id).map_or(&[], Vec::as_slice)
    }

    /// The selected extension, if there is one.
    pub(crate) fn chosen(&self) -> Option<&str> {
        self.rows.get(self.cursor).map(|f| f.id.as_str())
    }

    /// Moves the cursor and DISCARDS the detail card: it now describes a
    /// different extension.
    pub(crate) fn mover(&mut self, delta: i64) {
        let total = self.total();
        if total == 0 {
            return;
        }
        let target = i64::try_from(self.cursor)
            .unwrap_or(0)
            .saturating_add(delta);
        let new_cursor = usize::try_from(target.max(0)).unwrap_or(0).min(total - 1);
        if new_cursor != self.cursor {
            self.cursor = new_cursor;
            self.close_detail();
        }
    }

    /// Points the cursor at a specific row (a click). Out of range does
    /// nothing: the painter can be one frame behind.
    pub(crate) fn point_at(&mut self, row: usize) {
        if row < self.total() && row != self.cursor {
            self.cursor = row;
            self.close_detail();
        }
    }

    /// Closes the detail card and forgets what was requested.
    pub(crate) fn close_detail(&mut self) {
        self.detail = None;
        self.requested = None;
    }

    /// `true` if there is an open detail card to close.
    pub(crate) fn has_detail(&self) -> bool {
        self.detail.is_some()
    }

    /// Claims the detail card of the selected extension, if not already
    /// requested.
    pub(crate) fn claim_detail(&mut self) -> Option<String> {
        let id = self.chosen()?.to_owned();
        if self.requested.as_deref() == Some(id.as_str()) {
            return None;
        }
        self.requested = Some(id.clone());
        Some(id)
    }

    /// Installs the detail card the daemon answered with.
    ///
    /// Discarded if the reader has already moved to another row: a slow
    /// response cannot describe an extension other than the one selected.
    pub(crate) fn set_detail(&mut self, id: &str, res: &PluginGetConfigResult, lang: Lang) {
        if self.requested.as_deref() != Some(id) {
            return;
        }
        // The shared EDITOR is the model, not a projected list: it is the
        // one that knows a `bool` cycles, an `int` is typed and validated
        // against its bounds, and a `kind` this build does not know — a
        // newer peer — is read-only instead of a panic.
        self.detail = Some(Detail {
            id: id.to_owned(),
            state: PluginConfigState::new(sanitize_config_keys(&res.keys)),
            commands: self.commands.get(id).cloned().unwrap_or_default(),
            lang,
        });
    }

    /// Moves the cursor INSIDE the detail card. `false` if there is no
    /// detail card — or if the one there has no keys to walk, in which case
    /// the arrows belong to the catalogue: a detail card with nothing to
    /// walk that kept the keys would leave the reader unable to move
    /// without closing it first.
    pub(crate) fn move_in_card(&mut self, delta: i64) -> bool {
        let Some(f) = self.detail.as_mut() else {
            return false;
        };
        if f.state.rows().is_empty() {
            return false;
        }
        // The shared model moves one at a time and clamps; a page is N of
        // its own steps, not an index computed here — which is how you end
        // up with two answers for where the cursor is.
        let steps = delta.unsigned_abs().min(f.state.rows().len() as u64);
        for _ in 0..steps {
            if delta < 0 {
                f.state.up();
            } else {
                f.state.down();
            }
        }
        true
    }

    /// `Enter` on the selected key: cycles a `bool`/`enum` — and then there
    /// is something to write — or opens the edit buffer of a `string`/`int`.
    ///
    /// An unknown `kind` does nothing, which is the shared model's answer:
    /// blindly editing a shape this build does not understand is writing
    /// into a plugin's `config.toml` something nobody can vouch for.
    pub(crate) fn activate_key(&mut self) -> Option<(String, PendingConfigWrite)> {
        let f = self.detail.as_mut()?;
        let write = f.state.activate()?;
        Some((f.id.clone(), write))
    }

    /// `true` if the detail card's edit buffer is open.
    pub(crate) fn editando(&self) -> bool {
        self.detail.as_ref().is_some_and(|f| f.state.is_editing())
    }

    /// A character into the edit buffer.
    pub(crate) fn write(&mut self, c: char) {
        if let Some(f) = self.detail.as_mut() {
            f.state.edit_push_char(c);
        }
    }

    /// Deletes the last character of the buffer.
    pub(crate) fn delete(&mut self) {
        if let Some(f) = self.detail.as_mut() {
            f.state.edit_backspace();
        }
    }

    /// Closes the buffer WITHOUT writing.
    pub(crate) fn cancel_edit(&mut self) {
        if let Some(f) = self.detail.as_mut() {
            f.state.edit_cancel();
        }
    }

    /// Commits the buffer: the value to write, or why it is not valid.
    ///
    /// # Errors
    /// Whatever the shared model says: it does not parse as an integer, or
    /// it parses and falls outside the schema's bounds.
    pub(crate) fn confirm_edit(
        &mut self,
    ) -> Option<Result<(String, PendingConfigWrite), SettingsEditError>> {
        let f = self.detail.as_mut()?;
        let id = f.id.clone();
        Some(f.state.edit_commit().map(|w| (id, w)))
    }

    /// The projection.
    pub(crate) fn vista(&self) -> ExtensionsView {
        ExtensionsView {
            rows: self.rows.clone(),
            cursor: self.cursor as u64,
            detail: self.detail.as_ref().map(Detail::view),
            loading: self.loading,
            errors: self.errors.clone(),
        }
    }
}

/// A third-party string ready to paint, and whether it differs from what it
/// says.
pub(crate) type Text = (String, bool);

/// What must be SHOWN before granting capabilities.
pub(crate) struct Grant {
    /// Whose they are.
    pub(crate) name: Text,
    /// What is granted, one per line.
    pub(crate) capabilities: Vec<Text>,
    /// The manifest anchor that was SHOWN (#282), if the peer sends it.
    ///
    /// The capability comparison `grant` does covers what is PAINTED;
    /// this one covers what is GRANTED, which is more: `category` and
    /// `contributions` — when and how the extension fires — go into the
    /// anchor and not into the list. And the local comparison only sees
    /// changes within this client: the `plugin.toml` that changes under the
    /// core is caught by the core, with this.
    pub(crate) digest: Option<String>,
}

/// The open detail card: WHO, its `[config]` editor and what commands it
/// contributes.
///
/// The editor is `norte_frontend::plugin_config::PluginConfigState`, the
/// same one that drives the TUI's manager. What cycles and what is typed is
/// not decided here: that would have two answers as soon as either of the
/// two surfaces changed.
struct Detail {
    /// Whose detail card it is.
    id: String,
    /// The shared editor over its keys.
    state: PluginConfigState,
    /// Its commands, already masked.
    commands: Vec<ExtensionCommandView>,
    /// Which language each key's domain was composed in.
    lang: Lang,
}

impl Detail {
    /// The detail card's projection.
    fn view(&self) -> ExtensionDetailView {
        ExtensionDetailView {
            id: self.id.clone(),
            config: self
                .state
                .rows()
                .iter()
                .map(|k| {
                    let domain = domain_of(k, self.lang);
                    ExtensionConfigRowView {
                        key: clamp_display(k.key.clone()),
                        kind: clamp_display(k.kind.clone()),
                        // On the PAINT side, never the operand: `k.value` is
                        // what an editor would write back.
                        value: clamp_display(k.display.value.clone()),
                        default: clamp_display(k.display.default.clone()),
                        description: clamp_display(k.description.clone()),
                        domain: clamp_display(domain),
                        hostile: k.display.hostile,
                        // It is decided by the SAME closed set the shared
                        // model knows how to edit. Without this, the screen
                        // offers `Enter` on a key that is not going to
                        // change and the reader concludes the write failed.
                        editable: k.is_editable(),
                    }
                })
                .collect(),
            commands: self.commands.clone(),
            cursor: self.state.cursor() as u64,
            editing: self.state.edit_buffer().map(|b| {
                // The buffer is painted SANITIZED — a human types it, but the
                // starting value was written by the plugin — and the operand
                // stays raw inside the shared model.
                clamp_display(norte_frontend::display_name(b.as_bytes()).0)
            }),
            editing_hostile: self
                .state
                .edit_buffer()
                .is_some_and(|b| norte_frontend::display_name(b.as_bytes()).1),
        }
    }
}

/// An extension's commands, already masked.
///
/// A command's `id` is NOT masked and NOT clamped: it is the dispatch key
/// that goes back to the daemon, and the manifest does not validate its
/// charset — that is why it is NEVER painted. What is painted is the title.
fn commands_of(p: &PluginInfo) -> Vec<ExtensionCommandView> {
    p.commands
        .iter()
        .map(|c| {
            let (title, hostile) = norte_frontend::display_name(c.title.as_bytes());
            ExtensionCommandView {
                id: c.id.clone(),
                title: clamp_display(title),
                hostile,
            }
        })
        .collect()
}

/// What bounds a key: an `enum`'s values, an `int`'s bounds, or nothing.
fn domain_of(k: &norte_frontend::plugin_config::ConfigKeyRow, lang: Lang) -> String {
    if !k.display.values.is_empty() {
        // The MASKED ones: an `enum` value is text the plugin writes, and
        // this `·` is an in-band composition.
        return k.display.values.join(" · ");
    }
    match (k.min, k.max) {
        (Some(min), Some(max)) => norte_i18n::ta_in(
            lang,
            "ext-config-range",
            &[("min", &min.to_string()), ("max", &max.to_string())],
        ),
        (Some(min), None) => {
            norte_i18n::ta_in(lang, "ext-config-min", &[("min", &min.to_string())])
        }
        (None, Some(max)) => {
            norte_i18n::ta_in(lang, "ext-config-max", &[("max", &max.to_string())])
        }
        (None, None) => String::new(),
    }
}

/// A catalogue row, sanitized.
fn row_from(p: &norte_proto::methods::PluginInfo) -> ExtensionRowView {
    let name = plugin_label(&p.name);
    ExtensionRowView {
        // The id is NOT masked and NOT clamped: it is a validated key
        // (reverse-DNS), and either operation would break it — masking is
        // not injective and neither is clamping.
        id: p.id.clone(),
        name: clamp_display(if norte_help::is_blank_id(&name) {
            // A `name` made of invisible filler is a legal manifest whose
            // row paints blank: it falls back to the id, the only thing the
            // host assigns.
            plugin_label(&p.id)
        } else {
            name
        }),
        publisher: clamp_display(plugin_label(&p.publisher)),
        version: clamp_display(plugin_label(&p.version)),
        // The category is CORE vocabulary (`previewer`, `indexer`…), not
        // free text from the manifest: it still enters through the same
        // door, because it is the daemon that sends it, not this process.
        category: clamp_display(plugin_label(&p.category)),
        description: clamp_display(
            p.description
                .as_deref()
                .map(plugin_description)
                .unwrap_or_default(),
        ),
        approved: p.approved,
        enabled: p.enabled,
        has_help: p.has_help,
        commands: u32::try_from(p.commands.len()).unwrap_or(u32::MAX),
        columns: u32::try_from(p.columns.len()).unwrap_or(u32::MAX),
        capabilities: p
            .capabilities
            .iter()
            .map(|c| clamp_display(plugin_label(c)))
            .collect(),
    }
}

/// A string a THIRD PARTY wrote, ready to paint: masked, clamped, and with
/// the flag for whether what is painted differs from what it says.
///
/// It is `plugin_label` with its flag — the same function, not a copy —
/// plus this host's screen clamping. Where the decision IS the string
/// (approving a capability), the flag is part of the question.
pub(crate) fn third_party_text(raw: &str) -> (String, bool) {
    let (paintable, hostile) = plugin_label_flagged(raw);
    (clamp_display(paintable), hostile)
}

/// The string already carries the replacement from a lossy conversion
/// SOMEONE ELSE did.
///
/// norte never writes U+FFFD except as a mask, so finding it in something
/// that has not gone through the mask yet means someone upstream converted
/// bytes that were not UTF-8 and did not say so.
fn already_lossy_converted(s: &str) -> bool {
    s.contains('\u{fffd}')
}
