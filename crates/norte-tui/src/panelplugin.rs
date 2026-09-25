//! What a PLUGIN panel keeps alive in the terminal: its last frame, the
//! opaque state the guest saved, and which repaint is in flight (phase 3).
//!
//! The frame is described by the guest and painted by [`crate::ui`]; here it
//! is only stored. What IS decided here is WHEN what is there stops being
//! valid, and that is why the signature below exists.

use norte_frontend::frame::StyledFrame;
use norte_proto::VPath;

/// What makes one repaint different from another.
///
/// A panel does not repaint "every so often": it repaints when something the
/// guest would see changes. Comparing the signature of what is being looked
/// at with the one that was requested is what does the two things that
/// matter — not requesting the same thing twice, and DROPPING the response
/// that arrives once the slot already wants something else.
///
/// This is the previews' rule (`PreviewFetch` keeps its path), not an epoch
/// counter: here the identity is what was requested, and comparing it alone
/// says whether the response still holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Signature {
    /// WHICH panel it is: the whole kind, `plugin:<id>:<kind>`.
    ///
    /// It is in the signature because a `SlotId` gets REUSED: presets bring
    /// small, fixed ids, so changing layout can put another plugin's panel
    /// in the same slot. Without the kind, `hay_that_request` used to say
    /// nothing needed requesting — the directory and size matched — and the
    /// previous plugin's frame stayed painted, with its clickable zones,
    /// under the new one's title.
    pub kind: String,
    /// The directory the panel looks at.
    pub dir: VPath,
    /// Usable width in cells, without the frame's borders.
    pub cols: u32,
    /// Usable height in cells, without the borders.
    pub rows: u32,
    /// The name of the row under the listing's cursor that it follows, if
    /// there is one. It is in the signature because the guest receives it: a
    /// panel that talks about the pointed-at row has to repaint when another
    /// one is pointed at.
    pub cursor: Option<String>,
}

/// A plugin's panel, between repaints.
#[derive(Debug, Default)]
pub struct PanelRuntime {
    /// WHICH panel what is stored here belongs to.
    ///
    /// A `SlotId` gets reused — changing layout or restoring the session
    /// brings small, fixed ids — so the slot can go from one plugin to
    /// another. Pruning by the tree is not enough: the slot stays ALIVE,
    /// just now belonging to another one. Without this, A's stuff was
    /// inherited by B: its frame — with its clickable zones — under B's
    /// title, and its opaque state handed to B on the first request.
    pub kind: Option<String>,
    /// The last frame that arrived. Kept while the next one is requested: a
    /// slow plugin leaves the earlier snapshot, not a blank slot.
    pub frame: Option<StyledFrame>,
    /// The guest's opaque state, as is: it comes back on the next request
    /// and this process never looks at it.
    ///
    /// It survives the frame on purpose — it is the ONLY thing kept between
    /// calls; the read permission is not (the location session dies with
    /// every call, in the core).
    pub state: Option<Vec<u8>>,
    /// The signature of the frame currently being shown.
    pub shown: Option<Signature>,
    /// The signature of the request in flight, if there is one.
    pub in_flight: Option<Signature>,
    /// The last signature that was ATTEMPTED and came back without a frame:
    /// the RPC failed, or no consented plugin paints that panel.
    ///
    /// Without this, an empty attempt left no trace — `shown` stayed as
    /// it was and `in_flight` got cleared — so the loop's next round asked
    /// for the same thing again: one RPC per painted frame, forever, with
    /// nothing on screen to explain it. And it happens without hostile
    /// plugins: a saved layout naming a panel from a plugin that is no
    /// longer there is enough.
    pub attempted: Option<Signature>,
}

impl PanelRuntime {
    /// Puts this slot in the service of `kind`, dropping whatever belonged
    /// to another one.
    ///
    /// A `SlotId` gets REUSED: changing layout or restoring the session
    /// bring small, fixed ids, so slot 3 can belong to one plugin today and
    /// another a second later. Pruning by the tree does not cover it — the
    /// slot stays alive, just now belonging to another one — and what was
    /// there is not inherited: not the frame, because its clickable zones
    /// would keep responding under the new one's title, nor the OPAQUE
    /// STATE, which belongs to the first one and whose consent the reader
    /// gave plugin by plugin.
    ///
    /// Returns whether a handoff happened, for whoever wants to say so.
    pub fn adoptar(&mut self, kind: &str) -> bool {
        if self.kind.as_deref() == Some(kind) {
            return false;
        }
        *self = Self {
            kind: Some(kind.to_owned()),
            ..Self::default()
        };
        true
    }

    /// Does `signature`'s frame need requesting?
    ///
    /// No, if that same one is already being shown, and no, if it was
    /// already requested: a panel that repeats on every frame would make one
    /// call to the guest per paint.
    #[must_use]
    pub fn hay_that_request(&self, signature: &Signature) -> bool {
        // Nor what was already attempted and came back empty: a panel with
        // no plugin to paint it is not a panel that needs re-requesting on
        // every frame. When something the guest would see changes, the
        // signature will be a different one and it will be tried again.
        self.shown.as_ref() != Some(signature)
            && self.in_flight.as_ref() != Some(signature)
            && self.attempted.as_ref() != Some(signature)
    }
}

/// Requests the visible plugin panel's frame, if needed (phase 3).
///
/// Called once per paint turn, which is what gives it its cadence: there is
/// no timer nor coalescer — there is ONE live request per slot and the next
/// one replaces the previous, dropping its receiver — the same rule as the
/// docked preview. What decides whether it is needed is the signature, not
/// the clock.
pub fn request_marco(
    app: &mut crate::app::App,
    backend: &norte_core::backend::Backend,
    work: &mut crate::jobs::InFlight,
    painted: ratatui::layout::Rect,
) {
    // The guards BEFORE the geometry: resolving the tree is a whole layout
    // pass, and a screen with no plugin panel at all — which is almost every
    // screen — should not pay for it per frame.
    let Some(slot) = app.panel_slot() else {
        return;
    };
    let Some(kind) = app.layout.kind_of(slot).map(|k| k.as_str().to_owned()) else {
        return;
    };
    let Some((plugin_id, panel_kind)) = parts(&kind) else {
        return;
    };
    // And one live request per slot: while one is in flight, another does
    // not start. Dropping the receiver used to discard the RESPONSE, not the
    // work — the guest instantiates and runs regardless — so dragging a
    // border queued one wasm instantiation per frame. The loop's next round
    // looks again, so what is lost is one round, not the repaint.
    if app.panels.entry(slot).in_flight.is_some() {
        return;
    }
    let res = crate::ui::resolved_for(app, painted);
    let Some(rect) = crate::ui::slot_rect(&res, slot) else {
        return;
    };
    let rect = crate::ui::slot_content(&app.layout, slot, rect);
    let signature = Signature {
        kind: kind.clone(),
        dir: app.focused().dir().clone(),
        // Without the borders: the guest describes what is INSIDE, and
        // giving it the size with the frame would make it count two cells
        // that are not its own.
        cols: u32::from(rect.width.saturating_sub(2)),
        rows: u32::from(rect.height.saturating_sub(2)),
        // `cursor_entry` and not `selected`, for the same reason as the
        // docked viewer and the attributes sheet: the panel talks about
        // what is UNDER the cursor, not what is marked.
        // By `display_name` and not `from_utf8_lossy`: it is the
        // sanitization everything painted uses, so the guest receives the
        // name this house would show. And it tells apart two names that
        // only differ in invalid bytes, which the lossy conversion
        // collapsed — with it, moving the cursor between those two did not
        // change the signature and the panel did not repaint.
        cursor: app.focused().cursor_entry().and_then(|e| {
            e.path
                .file_name()
                .map(|n| norte_frontend::display_name(n.as_bytes()).0)
        }),
    };
    // And that the kind be DECLARED by a consented plugin: the prefix is
    // written by whoever edits a layout, and without this gate a
    // `plugin:whatever:whatever` in a file was enough to ask the core to
    // resolve it with the directory the reader is looking at.
    if !app.kinds.decls().iter().any(|d| d.id.as_str() == kind) {
        return;
    }
    app.panels.entry(slot).adoptar(&kind);
    if !app.panels.entry(slot).hay_that_request(&signature) {
        return;
    }
    let params = norte_proto::methods::PluginPanelRenderParams {
        plugin_id: plugin_id.to_owned(),
        kind: panel_kind.to_owned(),
        dir: signature.dir.clone(),
        cols: signature.cols,
        rows: signature.rows,
        lang: norte_frontend::frame::lang_code().to_owned(),
        cursor_name: signature.cursor.clone(),
        // What the guest saved last time, as is: this process does not look
        // at it.
        state: app.panels.entry(slot).state.clone(),
        // ALWAYS the neutral event, today: no frontend sends `Click` or
        // `Command` to the guest yet. A clicked zone runs a catalogue
        // command (`mouse::pane_zone_in`) and the guest never finds out;
        // letting it react to its own zones is what is missing, and this is
        // the line for it.
        event: norte_proto::methods::PanelEvent::Refresh,
    };
    app.panels.entry(slot).in_flight = Some(signature.clone());
    work.panel_render = Some(crate::probes::spawn_panel_render(
        backend, slot, signature, params,
    ));
}

/// Applies the frame that came back from the core, if it still holds.
///
/// Three ways not to apply it, and all three leave whatever was there:
/// - the response is to an OLD request (the slot already wants something
///   else),
/// - the call failed,
/// - no consented plugin paints that panel.
///
/// In all three cases the previous frame is kept: a slow or broken plugin
/// leaves the earlier snapshot, never a slot flickering to empty.
pub fn land(
    app: &mut crate::app::App,
    slot: norte_frontend::layout::SlotId,
    signature: &Signature,
    res: Option<Result<Option<norte_proto::methods::PanelFrame>, norte_proto::Error>>,
) {
    let panel = app.panels.entry(slot);
    // A response to a request that is no longer the live one is neither
    // applied nor does it clear anything: the one in flight is a different
    // one and it is the one that rules.
    if panel.in_flight.as_ref() != Some(signature) {
        return;
    }
    panel.in_flight = None;
    // The attempt is recorded NO MATTER WHAT: this is what stops a panel
    // with no frame from being retried on every paint.
    panel.attempted = Some(signature.clone());
    let Some(Ok(Some(marco))) = res else {
        return;
    };
    // And that WHOEVER was asked is the one signing it: the frame says which
    // plugin it belongs to, and a `plugin_id` that is not this signature's
    // kind's is not painted. With the signature carrying the kind this
    // should not be able to happen; it is checked because the data comes
    // from outside and checking it costs one line.
    if parts(&signature.kind).map(|(id, _)| id) != Some(marco.plugin_id.as_str()) {
        return;
    }
    panel.frame = Some(marco_de_wire(&marco));
    panel.state = marco.state;
    panel.shown = Some(signature.clone());
}

/// The wire frame, BOUNDED and SANITIZED, in the shape the terminal paints.
///
/// Via `clamped` and not field by field: the caps are the protocol's and the
/// trim drops zones that point to a line that is not painted. A hostile
/// guest sends a thousand lines and ten thousand zones just as an honest one
/// sends eight.
///
/// And via [`norte_frontend::ansi::span_de_wire`] and not copying the
/// fields: a span's text belongs to a THIRD PARTY and is masked the same way
/// a styled preview's is, and its `role` is validated against what a plugin
/// is allowed to request. Copying them by hand — as it was — let terminal
/// escapes and chrome roles through the one path nobody had looked at.
fn marco_de_wire(marco: &norte_proto::methods::PanelFrame) -> StyledFrame {
    StyledFrame::de_wire(marco)
}

/// `plugin:<id>:<kind>` split into the two halves the RPC needs.
///
/// The separator is the FIRST `:` after the prefix, and that is safe because
/// the alphabet `KindRegistry::insert_panels` validates against does not let
/// a colon through in either the id or the kind: without that gate, a plugin
/// called `a:b` could pass itself off as another one's panel.
///
/// ```
/// # use norte_tui::panelplugin::parts;
/// assert_eq!(parts("plugin:git:status"), Some(("git", "status")));
/// assert_eq!(parts("browser"), None);
/// ```
#[must_use]
pub fn parts(kind: &str) -> Option<(&str, &str)> {
    kind.strip_prefix("plugin:")?.split_once(':')
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signature(dir: &str, cursor: Option<&str>) -> Signature {
        Signature {
            kind: "plugin:git:status".to_owned(),
            dir: VPath::parse(dir).expect("wire"),
            cols: 30,
            rows: 8,
            cursor: cursor.map(str::to_owned),
        }
    }

    /// What is already being shown is not requested again, and neither is
    /// what was already requested.
    ///
    /// Without this, the terminal repaints on every loop turn and every turn
    /// would be a call to the guest: a git panel running wasm sixty times a
    /// second to show the same branch.
    #[test]
    fn what_is_already_shown_or_requested_is_not_requested_again() {
        let f = signature("mem:///a", None);
        let mut p = PanelRuntime::default();
        assert!(
            p.hay_that_request(&f),
            "with nothing yet, it must be requested"
        );

        p.in_flight = Some(f.clone());
        assert!(!p.hay_that_request(&f), "already requested");

        p.in_flight = None;
        p.shown = Some(f.clone());
        assert!(!p.hay_that_request(&f), "already being shown");
    }

    /// Moving the cursor changes the signature: the guest receives the
    /// pointed-at row, so pointing at another one is a different frame.
    #[test]
    fn moving_the_cursor_requests_another_frame() {
        let p = PanelRuntime {
            shown: Some(signature("mem:///a", Some("one"))),
            ..Default::default()
        };
        assert!(p.hay_that_request(&signature("mem:///a", Some("two"))));
        assert!(p.hay_that_request(&signature("mem:///b", Some("one"))));
    }

    /// An attempt that comes back EMPTY is not repeated on the next frame.
    ///
    /// Without this, a panel whose plugin is no longer there — a saved
    /// layout that names it — requested a frame on every paint: one RPC per
    /// frame, and embedded in it a disk scan of the catalogue.
    #[test]
    fn an_empty_attempt_is_not_repeated_on_every_frame() {
        let f = signature("mem:///a", None);
        let mut p = PanelRuntime {
            in_flight: Some(f.clone()),
            ..Default::default()
        };
        p.in_flight = None;
        p.attempted = Some(f.clone());
        assert!(
            !p.hay_that_request(&f),
            "already tried and came back with no frame"
        );
        // But whatever changes the context IS requested: the plugin could
        // have come back, and either way the guest would see something
        // else.
        assert!(p.hay_that_request(&signature("mem:///b", None)));
    }

    /// A slot that becomes ANOTHER plugin's inherits nothing from the
    /// previous one.
    ///
    /// Not the frame — its clickable zones would keep responding under the
    /// new one's title — nor the opaque state, which belongs to the first
    /// one. Happens on a layout change or a session restore, because a
    /// preset's slot ids are small and fixed.
    #[test]
    fn a_slot_switching_plugins_inherits_nothing() {
        let mut p = PanelRuntime::default();
        assert!(p.adoptar("plugin:git:status"), "claims a fresh slot");
        p.frame = Some(StyledFrame::clamped(Vec::new(), Vec::new()));
        p.state = Some(b"git's stuff".to_vec());
        p.shown = Some(signature("mem:///a", None));

        assert!(
            !p.adoptar("plugin:git:status"),
            "the same panel does not hand off"
        );
        assert!(p.state.is_some(), "and does not drop its own stuff");

        assert!(
            p.adoptar("plugin:otro:cosas"),
            "another plugin does hand off"
        );
        assert!(
            p.state.is_none(),
            "the first one's opaque state is not inherited"
        );
        assert!(p.frame.is_none(), "nor its frame, with its zones");
        assert!(p.shown.is_none(), "and it requests again");
    }

    /// A house kind is not split: it belongs to no plugin.
    #[test]
    fn only_plugin_kinds_are_split() {
        assert_eq!(
            parts("plugin:acme.git:status"),
            Some(("acme.git", "status"))
        );
        assert_eq!(parts("plugin:sinkind"), None);
        assert_eq!(parts("logview"), None);
    }
}
