//! The dialogs as seen from dispatch: what commands each modal accepts (the
//! `ALLOW_*`), what an already-resolved `dialog.*` translates to, and the
//! two decisions that don't go through there (help and `init.lua` trust).

use super::modal::{DialogOutcome, Modal};

/// ALLOWLIST for `Modal::ConfirmDelete`/`Modal::ConfirmTransfer`/
/// `Modal::ConfirmQuit` (S2, `[ui] confirm_quit`): `approve` and `confirm`
/// both accept (Enter and `y` work the same as before H1), `deny`/`cancel`
/// refuse. Deliberately excludes the collision/approval commands — a rebind
/// of `w`→`dialog.newer` does nothing here.
pub const ALLOW_CONFIRM: &[&str] = &[
    "dialog.approve",
    "dialog.confirm",
    "dialog.deny",
    "dialog.cancel",
];

/// ALLOWLIST for `Modal::ConfirmPluginUninstall` (ADR 0104): like
/// [`ALLOW_CONFIRM`] but WITHOUT `dialog.approve`. That's the key the reader
/// just pressed in the list to grant capabilities, and over this modal it
/// deletes files; on this host only `confirm` is affirmative, and the
/// modal's footer must not offer a key that means nothing here.
pub const ALLOW_UNINSTALL: &[&str] = &["dialog.confirm", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST for `Modal::Collision`: overwrite/skip/rename/newer choose a
/// policy and retry; `cancel` closes. DELIBERATELY excludes
/// `dialog.confirm`/`dialog.approve` — there's no innocuous answer Enter
/// should fire on its own (decision 4 of plan H1, same as before H1).
pub const ALLOW_COLLISION: &[&str] = &[
    "dialog.overwrite",
    "dialog.skip",
    "dialog.rename",
    "dialog.newer",
    "dialog.cancel",
];

/// ALLOWLIST for `Modal::ApproveAgentOp`: ONLY `approve` confirms; `deny`
/// and `cancel` deny (closing IS denying, fail-safe). DELIBERATELY excludes
/// `dialog.confirm` — approving an AGENT mutation isn't an innocuous answer
/// Enter should fire on its own (decision 2 of plan H1).
pub const ALLOW_APPROVAL: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST for `Modal::TrustHostKey` (SSH TOFU, #45): same principle as
/// [`ALLOW_APPROVAL`] — ONLY `approve` trusts, `dialog.confirm` deliberately
/// excluded (Enter never trusts an unverified host key).
pub const ALLOW_TRUST_HOST: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST for `Modal::AskSecret` (#325). Here `dialog.confirm` DOES get
/// in, unlike the three above, and the difference is what's being asked:
/// they ask for a JUDGMENT about something the user didn't type — a
/// fingerprint, an agent op, some capabilities — where a reflexive Enter
/// approves without having looked. This one asks for a VALUE the user just
/// typed, and already decided on by typing it.
///
/// `dialog.approve` stays out for the opposite reason: delivering a
/// password isn't approving anything, and offering the approve key here
/// would suggest that's what it's for.
///
/// With an EMPTY field, confirming stays inert — the guard is inside
/// [`dialog_action`], which returns `None` and leaves the dialog open (the
/// same mechanism as a non-applicable batch plan). Both halves matter:
/// delivering the empty string would reproduce what #320 closed — an empty
/// secret leaves the provider taking credentials from the environment — and
/// closing would force redoing the whole navigation over one extra Enter,
/// which in a field where what's typed isn't shown is the easy mistake to
/// make. Cancel stays alive: leaving IS an answer.
pub const ALLOW_ASK_SECRET: &[&str] = &["dialog.confirm", "dialog.cancel"];

/// ALLOWLIST for `Modal::ConfirmPluginApproval` (#280): same principle as
/// [`ALLOW_APPROVAL`] — granting capabilities to an extension IS that
/// system's security decision, and `dialog.confirm` stays out on purpose:
/// Enter doesn't grant permission to read anyone's files.
pub const ALLOW_PLUGIN_APPROVAL: &[&str] = &["dialog.approve", "dialog.deny", "dialog.cancel"];

/// ALLOWLIST for the theme picker (`on_theme_picker_key`, main.rs): no
/// security risk (choosing a theme mutates nothing outside the popup
/// itself), so `confirm` DOES fire (unlike the modals above). This
/// overlay's only list — dispatch (main.rs) and the generated hint (H1 T3,
/// `hints::DialogHints`) share it, never a copy.
pub const ALLOW_PICKER: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST for the columns picker (#108 7a, `on_columns_key`, main.rs) —
/// the single source for dispatch and for the footer's generated hint
/// (`hints::DialogHints::columns`), the #24 pattern. `confirm` DOES
/// apply+persist (same criterion as [`ALLOW_PICKER`]: choosing columns only
/// touches its own config, it isn't a data mutation Enter has to protect).
pub const ALLOW_COLUMNS: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.toggle-enabled",
    "dialog.move-up",
    "dialog.move-down",
    "dialog.sort",
    "dialog.cycle-format",
    "dialog.confirm",
    "dialog.cancel",
];

/// ALLOWLIST for the extensions manager (`on_extensions_key`, main.rs,
/// M4-P3): `approve` toggles the plugin's approval (decision 3 of plan H1 —
/// "approving a plugin" reuses `dialog.approve`), `toggle-enabled`
/// enables/disables it. `confirm` (G3c) opens the highlighted plugin's
/// `[config]` section, IF it declares any key — Enter never approves (P1's
/// pin), it only enters a submenu. Shared by dispatch and the generated
/// hint.
pub const ALLOW_EXTENSIONS: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.approve",
    "dialog.toggle-enabled",
    // Uninstall (ADR 0104): the verb that removes an entry in the favorites
    // list here removes the whole extension, and that's why it asks.
    "dialog.remove",
    "dialog.confirm",
    "dialog.cancel",
    // `tab` moves focus between the list and the card's BUTTONS, which
    // until now only the mouse could press as buttons (ADR 0104 brought
    // them with the card). Each keeps its own key; this is the path for
    // whoever walks the screen instead of remembering five letters.
    "dialog.pane",
];

/// ALLOWLIST for a plugin's `[config]` panel (G3c, `on_extensions_key` when
/// `mgr.config.is_some()` and a buffer is NOT being edited — while editing,
/// keys get captured RAW, same criterion as `on_nav_popup_key`'s
/// `name_input`): `up`/`down` move the cursor over the keys, `confirm`
/// cycles a `bool`/`enum` or opens editing a `string`/`int`, `cancel`
/// closes the panel (returns to the plugin list, does NOT close the whole
/// overlay).
pub const ALLOW_PLUGIN_CONFIG: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
    // `tab` leaves the panel like `Esc`: it's entered with `tab` from the
    // button ring, and leaving with the same key is what's expected.
    "dialog.pane",
];

/// ALLOWLIST for the places sidebar (L3, `on_places_key` in main.rs).
///
/// The same `dialog.*` vocabulary all seven presets already bind: a panel
/// that moves with arrows and confirms with Enter needs no vocabulary of
/// its own, and giving it one would have meant seven presets touched by one
/// new key. `toggle-enabled` folds the section, `cancel` hands the keyboard
/// back to the listings WITHOUT closing the sidebar — closing it is
/// `layout.places`'s job.
pub const ALLOW_PLACES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.toggle-enabled",
    "dialog.cancel",
    // Width: with the keyboard INSIDE, `layout.grow`/`shrink` change THIS
    // panel's width. It's the only path through which they can —
    // `layout_resize`'s caller always passes a visible listing (#244 M1).
    "layout.grow",
    "layout.shrink",
    // Its OWN key, which is why it's bound in `[global]`: without it the
    // sidebar keeps `alt+b` and can't close itself — you'd open the panel
    // and that same key would stop existing. Piloting the TUI in tmux with
    // the whole suite green uncovered it, which is exactly what the harness
    // is for.
    "layout.places",
    // The disk map (phase 4): this list is ALSO filtered by the tree's keys
    // (`side_nav`), so without this `alt+z` died while inside the sidebar or
    // the tree — the two panels from which asking where the space went is
    // most tempting.
    "layout.disk-map",
    // `Tab` leaves to the listings without closing the panel. Opening a
    // side column left dead the key you've always switched panels with: the
    // panel swallows anything not listed here.
    //
    // Both VERBS, not one: on the `dialog` screen the presets bind `tab` to
    // `dialog.pane` — which is what "the other panel" is called in a dialog
    // — and `pane.switch` is what it's called on the navigation one.
    // Accepting only the second left the fix without effect with the
    // presets as they ship.
    "dialog.pane",
    "pane.switch",
    // And the ring, which is what `Tab` does NOT do: `pane.switch` hands the
    // keyboard back to the listings, while this moves to whichever panel is
    // next door. Without these two, the ring key died right inside the
    // panel it's meant to get you out of.
    "layout.focus-next",
    "layout.focus-prev",
    // And the OTHER panels' keys, by the same rule that already brought
    // this panel's own and the listing-switch one here: opening a side
    // column must not kill the key that opens the one next to it. Before,
    // with the keyboard in the sidebar, `alt+j` opened nothing and there was
    // no way to know why.
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "pane.tree",
    // And the APPLICATION's chrome, which isn't the listings': with the
    // keyboard inside the sidebar or the tree, `alt+m` didn't open the menu
    // bar and there was no way to know why — the panel swallowed the key.
    // `App::panel_chrome_command` dispatches it, one for all three.
    "app.menu",
    // And quitting, the key that can die worst inside a panel: the reader
    // closed the terminal believing they'd quit and the process stayed
    // alive with the session lock. The chrome handles it
    // (`App::panel_chrome_command`), honoring `[ui] confirm_quit`.
    "app.quit",
];

/// ALLOWLIST for the processes panel (`on_processes_key` in main.rs).
///
/// The same `dialog.*` vocabulary as the sidebar, for the same reason: a
/// panel that moves with arrows and acts with Enter needs no vocabulary of
/// its own, and giving it one would be seven presets touched by one new
/// key. `confirm` CANCELS the task under the cursor — it's the only action
/// the protocol has over a task — `cancel` hands the keyboard back to the
/// listings without closing the panel, and `layout.processes` closes from
/// inside.
///
/// It's TWO keypresses and not three: opening this panel ALREADY gives it
/// the keyboard, so the next one closes it. The three-press sequence — open,
/// focus, close — belongs to the docked viewer, and there it's deliberate
/// for a reason that doesn't apply here: the viewer follows the listing's
/// cursor, so giving it the keyboard on open would turn off the one thing
/// it does. This panel follows nothing.
pub const ALLOW_PROCESSES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.cancel",
    "layout.grow",
    "layout.shrink",
    "layout.processes",
    // Same as the sidebar: `Tab` hands the keyboard back to the listings,
    // with the two names that key has depending on the screen, and the ring
    // moves to the panel next door.
    "dialog.pane",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    // And the other panels', same as the sidebar.
    "layout.places",
    "layout.preview",
    "layout.metadata",
    "pane.tree",
    "layout.disk-map",
    // And the application's chrome, for the same reason as the sidebar.
    "app.menu",
    // And quitting, the key that can die worst inside a panel: the reader
    // closed the terminal believing they'd quit and the process stayed
    // alive with the session lock. The chrome handles it
    // (`App::panel_chrome_command`), honoring `[ui] confirm_quit`.
    "app.quit",
];

/// ALLOWLIST for the log panel (#323).
///
/// Its own and NOT the processes one, even though the two panels look
/// alike: there, `dialog.confirm` **cancels the task under the cursor**,
/// and an `Enter` in a log viewer that cancels a copy is exactly the class
/// of accident an allowlist exists to prevent. Here there's nothing to
/// confirm.
///
/// It also carries no `dialog.up`/`down`: the arrows, the pages, `End` and
/// `Esc` are claimed by the panel itself before the keymap
/// ([`crate::logview::key`]), because they're its own while it holds the
/// keyboard.
///
/// What it does carry is the chrome: closing from inside with the same key
/// that opened it, switching panels, resizing and the menu. Without
/// `layout.log` in this list, `alt+l` died in the funnel and the panel
/// couldn't be closed with the key that opened it — which is how this list
/// was found to be needed.
pub const ALLOW_LOG: &[&str] = &[
    "layout.log",
    "layout.grow",
    "layout.shrink",
    "dialog.pane",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "pane.tree",
    "layout.disk-map",
    "app.menu",
    // And quitting, the key that can die worst inside a panel: the reader
    // closed the terminal believing they'd quit and the process stayed
    // alive with the session lock. The chrome handles it
    // (`App::panel_chrome_command`), honoring `[ui] confirm_quit`.
    "app.quit",
];

/// ALLOWLIST for the disk map (phase 4).
///
/// Its own, like the log's and for the same reason: without
/// `layout.disk-map` in its own list, `alt+z` dies in the funnel and the
/// panel can't be closed with the key that opened it — which is how these
/// lists were found to be needed.
///
/// It carries no `dialog.up`/`down` nor `dialog.confirm`: the arrows, the
/// pages, `End`, `Home`, `Enter`, `r` and `Esc` are claimed by the panel
/// itself before the keymap ([`crate::diskmap::key`]), because they're its
/// own while it holds the keyboard. And `dialog.confirm` here WOULD enter a
/// directory: letting it through to the funnel would be an `Enter` with two
/// meanings depending on who's looking.
pub const ALLOW_DISK_MAP: &[&str] = &[
    "layout.disk-map",
    "layout.grow",
    "layout.shrink",
    "dialog.pane",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    // And the other panels': being in the map must not disable the key that
    // opens the column next door.
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "layout.log",
    "pane.tree",
    "app.menu",
    // And quitting, for the same reason as the log's: the key that can die
    // worst inside a panel.
    "app.quit",
];

/// ALLOWLIST for a panel CONTRIBUTED by a plugin (phase 3).
///
/// CHROME only, on purpose: while the guest doesn't receive commands (T4), a
/// plugin panel holding the keyboard has nothing of its own to do with a
/// key. Without this list, the keys fell through to `browse`'s resolver and
/// acted on the LISTING behind it while the focus border said the keyboard
/// was in the panel — #243's bug, and worse here: processes and the log
/// filter through their own allowlist, so there the destructive keys were
/// already out; a panel with no funnel left `F8` alive over the listing's
/// selection.
///
/// `dialog.cancel` releases the keyboard and does NOT close anything, as in
/// processes: a layout put the panel there, not this keypress.
pub const ALLOW_PANEL: &[&str] = &[
    "layout.grow",
    "layout.shrink",
    "dialog.cancel",
    "dialog.pane",
    "pane.switch",
    "layout.focus-next",
    "layout.focus-prev",
    "layout.places",
    "layout.preview",
    "layout.processes",
    "layout.metadata",
    "layout.log",
    "layout.disk-map",
    "pane.tree",
    "app.menu",
    // Quitting, for the same reason as the log's: the key that can die
    // worst inside a panel.
    "app.quit",
];

/// DISPATCH ALLOWLIST for the navigation popup (`on_nav_popup_key`,
/// main.rs), the union of what History, Hotlist and Volumes accept:
/// `add`/`remove` are filtered by the caller to `kind == Hotlist` (nothing
/// to name or delete in history or volumes) and `toggle-enabled` to
/// `kind == Volumes` (the "show all" toggle means nothing in the other two)
/// — same criterion as before H1.
///
/// The printed HINT is narrower than this per kind: [`ALLOW_NAV_HOTLIST`]
/// and [`ALLOW_NAV_VOLUMES`] are what each footer actually paints (design §D
/// — the volumes footer must not offer "add"/"remove", which mean nothing
/// over a mounted volume).
pub const ALLOW_NAV_POPUP: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.add",
    "dialog.remove",
    "dialog.toggle-enabled",
    "dialog.cancel",
    // Spec 2026-09-15 D2: open in the other panel (any list that navigates)
    // and clear (history and popular; filtered by the caller per kind).
    "dialog.confirm-other",
    "dialog.clear",
    // Filter history or popular entries (filtered by the caller per kind).
    "dialog.filter",
];

/// The popup's HINT in HISTORY or POPULAR mode (spec 2026-09-15 D2): add to
/// favorites, remove, clear, open in the other panel and filter.
pub const ALLOW_NAV_HISTORY: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.confirm-other",
    "dialog.add",
    "dialog.remove",
    "dialog.clear",
    "dialog.filter",
    "dialog.cancel",
];

/// The popup's HINT in HOTLIST mode (H1 T3): history and volumes paint their
/// own (or none) — see [`ALLOW_NAV_POPUP`] for why they're split apart.
pub const ALLOW_NAV_HOTLIST: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.add",
    "dialog.remove",
    "dialog.cancel",
];

/// The popup's HINT in VOLUMES mode (design §D): navigation, confirm,
/// cancel and the "show all" toggle — no `add`/`remove`.
pub const ALLOW_NAV_VOLUMES: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.confirm",
    "dialog.toggle-enabled",
    "dialog.cancel",
];

/// Verbs the help overlay dispatches (H3b). Navigation, confirm (run the
/// focused row or follow the focused link), cancel (close), plus its own
/// three. Nothing that mutates: the overlay itself changes no files — a
/// command it RUNS goes through the normal dispatch, with its own
/// confirmation, gate and journal entry.
pub const ALLOW_HELP: &[&str] = &[
    "dialog.up",
    "dialog.down",
    "dialog.page-up",
    "dialog.page-down",
    "dialog.top",
    "dialog.bottom",
    "dialog.section-prev",
    "dialog.section-next",
    "dialog.confirm",
    "dialog.cancel",
    "dialog.pane",
    "dialog.back",
    "dialog.filter",
];

/// Maps an already-resolved `dialog.*` command (by the effective `dialog`'s
/// [`Resolver`](crate::keymap::Resolver), H1 #24) to the active modal's
/// outcome, filtered by the concrete modal's ALLOWLIST: `None` = command
/// outside the allowlist, the key is INERT for this modal (e.g. Enter —
/// `dialog.confirm` — over an agent approval). The security semantics live
/// here, in code, never in the keymap: a rebind only changes which KEY
/// fires `dialog.approve`, never which modals accept `dialog.approve` as a
/// confirmation.
///
/// `Modal::TrustLuaInit` has no allowlist — decision 8 of plan H1, resolved
/// separately with [`trust_lua_key`] — and always returns `None` here.
// Modal→outcome table, one arm per variant and deliberately exhaustive:
// it's THE list of what each dialog accepts as an answer, and seeing it
// whole at once is the point. Splitting it would move the security
// semantics' boundary to an arbitrary spot and make it harder to see that
// no modal is missing — same criterion, and same exception, as
// `modal_title_body`.
#[expect(
    clippy::too_many_lines,
    reason = "modal→dialog table; same criterion as `modal_title_body`"
)]
#[must_use]
pub fn dialog_action(modal: &Modal, cmd: &str) -> Option<DialogOutcome> {
    use norte_proto::CollisionPolicy as P;
    match modal {
        // #139: properties don't ASK anything — they're read and closed —
        // so it only understands cancelling. Giving a "confirm" to a
        // read-only box teaches the reader that Enter does something here.
        Modal::Properties { .. } => (cmd == "dialog.cancel").then_some(DialogOutcome::Cancelled),
        // A batch's report doesn't ask either: it already happened. It
        // carries the confirm footer (`hints.confirm`), so EVERYTHING that
        // footer offers closes it — Enter is what gets pressed for
        // "understood", and there's nothing here to protect with it. A
        // painted button that does nothing would be worse than not painting
        // it.
        Modal::Report { .. } => match cmd {
            "dialog.approve" | "dialog.confirm" | "dialog.deny" | "dialog.cancel" => {
                Some(DialogOutcome::Cancelled)
            }
            _ => None,
        },
        // Checksums don't ask anything either: `confirm` COPIES the list to
        // the clipboard — the only thing that can be done with it — and
        // `cancel` closes. There's no mutation Enter has to protect.
        Modal::Checksums { .. } => match cmd {
            "dialog.confirm" => Some(DialogOutcome::Confirmed),
            "dialog.cancel" => Some(DialogOutcome::Cancelled),
            _ => None,
        },
        // Granting capabilities: `approve` grants and everything else on the
        // list closes without granting — closing IS not granting, fail-safe.
        Modal::ConfirmPluginApproval { .. } => {
            if !ALLOW_PLUGIN_APPROVAL.contains(&cmd) {
                return None;
            }
            Some(if cmd == "dialog.approve" {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled
            })
        }
        // M4-IA: `AiRenamePlan` is a decision surface over content the human
        // STARTED and REVIEWED — [`ALLOW_CONFIRM`] semantics (Enter
        // confirms, like a delete/transfer), NOT the agent-approval
        // allowlist (`ALLOW_APPROVAL`, which excludes confirm).
        //
        // With one caveat this separate arm exists to enforce (spec §17):
        // confirming needs an APPLICABLE batch plan. With no plan there's no
        // approved `plan_hash` to send, and with verdicts the core wouldn't
        // execute anything — in both cases the confirm key stays MUTE
        // (cancel stays alive), and the modal's footer stops offering it
        // (`modal-rename-batch-plan-hint-blocked`). Whether a plan can be
        // executed is the core's call: only `executable` gets read here.
        // The ORGANIZE plan (phase 8) with the SAME discipline: confirming
        // stays mute until the reader has reached the end of the tree. The
        // rule belongs to the shared crate for the reason its own comment
        // states — two approval criteria depending on the surface is the
        // worst kind of drift — and here it weighs more: this plan also
        // creates folders.
        Modal::OrganizePlan { seen, lines, .. } => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            let confirms = matches!(cmd, "dialog.approve" | "dialog.confirm");
            if confirms && !norte_frontend::approval_ready(true, *seen, lines.len()) {
                return None;
            }
            Some(if confirms {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled
            })
        }
        Modal::AiRenamePlan {
            plan,
            seen,
            entries,
            ..
        } => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            let confirms = matches!(cmd, "dialog.approve" | "dialog.confirm");
            // And that the reader HAS REACHED THE END, which is the half
            // that was missing here: a two-hundred-rename plan could be
            // approved having seen the first ten. The rule lives in the
            // shared crate because the window already required it, and an
            // approval with two different criteria depending on the surface
            // is the worst kind of drift (ADR 0077).
            if confirms && !norte_frontend::approval_ready(plan.confirmable(), *seen, entries.len())
            {
                return None;
            }
            Some(if confirms {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled // dialog.deny | dialog.cancel
            })
        }
        // Uninstalling an extension is a delete that asks, with delete-file
        // semantics (Enter confirms) — except `dialog.approve` isn't on the
        // list here: see [`ALLOW_UNINSTALL`]. Undoing up to a point (phase
        // 7) shares the allowlist and semantics with uninstalling, and for
        // the same reason: both accept a CONSEQUENCE over something that
        // already exists, so Enter confirms, Esc doesn't, and
        // `dialog.approve` isn't there — approve is the verb for granting
        // permissions, not for taking on an effect.
        Modal::ConfirmPluginUninstall { .. } | Modal::ConfirmUndoAfter { .. } => {
            if !ALLOW_UNINSTALL.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.confirm" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        // M4-IA-2: `SemanticHits` is likewise a decision surface over
        // content the human REQUESTED — Enter navigates to the hit under
        // the cursor, mutates nothing.
        Modal::ConfirmDelete { .. }
        | Modal::ConfirmTransfer { .. }
        | Modal::ConfirmQuit
        | Modal::SemanticHits { .. } => {
            if !ALLOW_CONFIRM.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" | "dialog.confirm" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        Modal::Collision { .. } => {
            if !ALLOW_COLLISION.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.overwrite" => DialogOutcome::Retry(P::Overwrite),
                "dialog.skip" => DialogOutcome::Retry(P::Skip),
                "dialog.rename" => DialogOutcome::Retry(P::RenameAuto),
                "dialog.newer" => DialogOutcome::Retry(P::Newer),
                _ => DialogOutcome::Cancelled, // dialog.cancel
            })
        }
        Modal::ApproveAgentOp { .. } => {
            if !ALLOW_APPROVAL.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        Modal::TrustHostKey { .. } => {
            if !ALLOW_TRUST_HOST.contains(&cmd) {
                return None;
            }
            Some(match cmd {
                "dialog.approve" => DialogOutcome::Confirmed,
                _ => DialogOutcome::Cancelled, // dialog.deny | dialog.cancel
            })
        }
        // #325: the ONE free-text modal that goes through here. The run
        // loop intercepts the others whole (raw chars) and they return
        // `None`; this one only intercepts typing and erasing, and leaves
        // Enter/Esc to this allowlist — because its Enter is a DECISION
        // (delivering the secret) and its Esc abandons a suspended
        // navigation, which is exactly what `cancel_prompt` refuses to do.
        Modal::AskSecret { input, .. } => {
            if !ALLOW_ASK_SECRET.contains(&cmd) {
                return None;
            }
            // Empty field = INERT confirm (see [`ALLOW_ASK_SECRET`]).
            if cmd == "dialog.confirm" && input.is_empty() {
                return None;
            }
            Some(if cmd == "dialog.confirm" {
                DialogOutcome::Confirmed
            } else {
                DialogOutcome::Cancelled
            })
        }
        // #103 T9: `MarkPattern` is free text, like the search dialog — the
        // run loop intercepts it BEFORE reaching here (raw chars, never the
        // `dialog` context), same as `TrustLuaInit`. Both always return
        // `None`.
        Modal::TrustLuaInit { .. }
        | Modal::MarkPattern { .. }
        | Modal::Mkdir { .. }
        | Modal::ProfileSaveAs { .. }
        | Modal::EditNew { .. }
        | Modal::CommandLine { .. }
        | Modal::AiRenameInstruction { .. }
        | Modal::RenameBatchPattern { .. }
        | Modal::SemanticQuery { .. }
        | Modal::TransferDest { .. }
        // #132: the two write-archive ones, for the same reason.
        | Modal::Pack { .. }
        | Modal::Split { .. }
        // #314: the permissions one is also free text — a mode gets typed.
        | Modal::Chmod { .. }
        | Modal::TransferName { .. } => None,
    }
}

/// What a `dialog.*` verb does inside the help overlay (H3b).
///
/// A vocabulary of its own rather than [`DialogOutcome`]: a modal answers a
/// QUESTION (confirm/deny/retry-with-a-policy) and this overlay is a reader —
/// its verbs move a cursor, follow a link and close a window. Sharing the
/// enum would force every modal to carry arms it can never produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpOutcome {
    /// One row up: a topic in the sidebar, an action in the body.
    Up,
    /// One row down.
    Down,
    /// A page up: topics in the sidebar, body lines in the body.
    PageUp,
    /// A page down.
    PageDown,
    /// The first topic, or the top of the body (`Home`).
    Top,
    /// The last topic, or the end of the body (`End`).
    Bottom,
    /// The previous section of the page (`[`).
    SectionPrev,
    /// The next section of the page (`]`).
    SectionNext,
    /// Enter: open the selected topic, or run/follow the focused body row.
    Activate,
    /// Close the overlay.
    Close,
    /// Hand the focus to the other half.
    TogglePane,
    /// Back to the previously open topic.
    Back,
    /// Start typing into the sidebar filter.
    StartFilter,
}

/// Maps an already-resolved `dialog.*` command to what it means inside the
/// help overlay, or `None` for a verb the overlay does not support — the key
/// is INERT, exactly as in [`dialog_action`].
///
/// Filtered through the SAME [`ALLOW_HELP`] the footer hint is generated from
/// ([`crate::hints::DialogHints::help`]), never a second copy: a verb the
/// footer advertises and dispatch drops (or the reverse) is a hint that lies,
/// and one list cannot drift from itself.
#[must_use]
pub fn help_action(cmd: &str) -> Option<HelpOutcome> {
    if !ALLOW_HELP.contains(&cmd) {
        return None;
    }
    Some(match cmd {
        "dialog.up" => HelpOutcome::Up,
        "dialog.down" => HelpOutcome::Down,
        "dialog.page-up" => HelpOutcome::PageUp,
        "dialog.page-down" => HelpOutcome::PageDown,
        "dialog.top" => HelpOutcome::Top,
        "dialog.bottom" => HelpOutcome::Bottom,
        "dialog.section-prev" => HelpOutcome::SectionPrev,
        "dialog.section-next" => HelpOutcome::SectionNext,
        "dialog.confirm" => HelpOutcome::Activate,
        "dialog.cancel" => HelpOutcome::Close,
        "dialog.pane" => HelpOutcome::TogglePane,
        "dialog.back" => HelpOutcome::Back,
        "dialog.filter" => HelpOutcome::StartFilter,
        // Unreachable through the allowlist above, and deliberately not an
        // `unreachable!`: a verb added to `ALLOW_HELP` without an arm here is
        // an inert key, never a panic in a reader's terminal. The test
        // `help_action_accepts_exactly_the_allowlist` is what catches it.
        _ => return None,
    })
}

/// Resolves the [`Modal::TrustLuaInit`] modal (decision 8 of plan H1: NOT
/// migrated to the `dialog` context — it's a SPECIAL resolution path the run
/// loop intercepts BEFORE consulting the keymap, because it needs the
/// `LuaHost` that only lives there). Same security contract as the rest of
/// the TOFU dialogs: `y` trusts, `n`/Esc deny, Enter does NOT decide (no
/// dangerous default that fires on its own).
#[must_use]
pub fn trust_lua_key(code: crossterm::event::KeyCode) -> DialogOutcome {
    use crossterm::event::KeyCode as K;
    match code {
        K::Char('y') => DialogOutcome::Confirmed,
        K::Char('n') | K::Esc => DialogOutcome::Cancelled,
        _ => DialogOutcome::Open,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::modal::quit_needs_confirm;
    use crate::app::testutil::*;
    use crate::app::trail::Trail;

    /// TOFU (#45): trusting is a security decision — `dialog.approve`
    /// trusts; `dialog.deny`/`dialog.cancel` cancel; `dialog.confirm`
    /// (Enter) is INERT (H1's safety pin: no dangerous default).
    #[test]
    fn trust_host_key_solo_approve_confia() {
        let m = Modal::TrustHostKey {
            host: "h".into(),
            port: Some(22),
            algo: "ssh-ed25519".into(),
            fingerprint: "SHA256:AAAA".into(),
            dir: root(),
            pane: 0,
            trail: Trail::Record,
        };
        assert_eq!(
            dialog_action(&m, "dialog.approve"),
            Some(DialogOutcome::Confirmed)
        );
        for cmd in ["dialog.deny", "dialog.cancel"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Cancelled));
        }
        assert_eq!(
            dialog_action(&m, "dialog.confirm"),
            None,
            "Enter (dialog.confirm) never trusts a host key"
        );
    }

    /// S2 (`[ui] confirm_quit`): `Modal::ConfirmQuit` reuses
    /// `ConfirmDelete`/`ConfirmTransfer`'s ALLOWLIST — `y`/Enter confirm
    /// (close), `n`/Esc cancel, any other command is out (`None`).
    #[test]
    fn confirm_quit_reutiliza_allow_confirm() {
        let m = Modal::ConfirmQuit;
        for cmd in ["dialog.approve", "dialog.confirm"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Confirmed));
        }
        for cmd in ["dialog.deny", "dialog.cancel"] {
            assert_eq!(dialog_action(&m, cmd), Some(DialogOutcome::Cancelled));
        }
        assert_eq!(
            dialog_action(&m, "dialog.overwrite"),
            None,
            "outside the confirm allowlist: inert"
        );
    }

    /// A batch report only CLOSES with everything its footer offers, and
    /// never "confirms" anything: there's nothing to confirm. Everything
    /// else, inert.
    #[test]
    fn el_informe_de_lote_solo_se_cierra() {
        let m = Modal::Report {
            kind: crate::app::ReportKind::Batch,
            lines: Vec::new(),
        };
        for cmd in [
            "dialog.approve",
            "dialog.confirm",
            "dialog.deny",
            "dialog.cancel",
        ] {
            assert_eq!(
                dialog_action(&m, cmd),
                Some(DialogOutcome::Cancelled),
                "{cmd}"
            );
        }
        assert_eq!(dialog_action(&m, "dialog.overwrite"), None);
    }

    /// S2 (`[ui] confirm_quit`): the three mode × in-flight-work
    /// combinations, each on its own (same style as the GUI's
    /// `has_pending_work_tasks_o_marcas_o_ninguno`).
    #[test]
    fn quit_needs_confirm_los_tres_modos() {
        use crate::config::ConfirmQuit;
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, true),
            "never NEVER confirms, even with work in flight"
        );
        assert!(
            !quit_needs_confirm(ConfirmQuit::Never, false),
            "never NEVER confirms"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, false),
            "always ALWAYS confirms, even with no pending work"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Always, true),
            "always ALWAYS confirms"
        );
        assert!(
            !quit_needs_confirm(ConfirmQuit::Auto, false),
            "auto with no pending work: closes right away"
        );
        assert!(
            quit_needs_confirm(ConfirmQuit::Auto, true),
            "auto with pending work: confirms (pre-S2 behavior)"
        );
    }

    /// Lua TOFU (M4, decision 8 of plan H1: NOT migrated): same security
    /// contract as the rest — only `y` trusts; `n` and Esc deny; Enter does
    /// NOT decide.
    #[test]
    fn trust_lua_init_solo_y_confia_y_enter_no_decide() {
        use crossterm::event::KeyCode as K;
        assert_eq!(trust_lua_key(K::Char('y')), DialogOutcome::Confirmed);
        assert_eq!(trust_lua_key(K::Char('n')), DialogOutcome::Cancelled);
        assert_eq!(trust_lua_key(K::Esc), DialogOutcome::Cancelled);
        assert_eq!(
            trust_lua_key(K::Enter),
            DialogOutcome::Open,
            "Enter never approves running someone else's script"
        );
    }
}
