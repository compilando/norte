//! The UI session as seen from the frontend (L2): what gets captured, what
//! gets applied, and what does NOT travel.
//!
//! The rule these tests pin is that capturing and applying are the same
//! screen said twice. Everything else is the three things decided on
//! purpose: marks do not travel, the state of a slot the layout does not
//! have is kept, and an unreadable body does not leave a blank screen.

use norte_frontend::layout::{KindId, Node, SlotId};
use norte_proto::VPath;
use norte_tui::app::{App, Pane};

fn vp(wire: &str) -> VPath {
    let _ = norte_i18n::force(norte_i18n::Lang::Es);
    VPath::parse(wire).expect("valid wire")
}

fn app_basica() -> App {
    App::new(
        Pane::new(vp("file:///izq"), Vec::new()),
        Pane::new(vp("file:///der"), Vec::new()),
    )
}

/// Capturing and reapplying leaves the same screen: same tree, same paths,
/// same cursor.
#[test]
fn capturar_y_aplicar_es_la_identidad() {
    let mut app = app_basica();
    app.set_layout(norte_frontend::layout::presets::tree("krusader").expect("preset"));
    let before = app.session_body();
    let mut other = app_basica();
    other.apply_session(&before);
    assert_eq!(other.session_body(), before);
}

/// This process looks at ONE profile and the document belongs to all of
/// them: what belongs to the others travels back intact.
///
/// Writing only the active one would erase from the body the place where
/// other profiles left their panels, and the reader would find out by going
/// back to one and finding it blank (ADR 0079, D5).
#[test]
fn las_disposiciones_de_otros_perfiles_vuelven_intactas() {
    let ajena = norte_frontend::layout::presets::tree("krusader").expect("preset");
    let mut body = norte_frontend::session::SessionBody::default();
    body.layouts.insert("photos".to_owned(), ajena.clone());

    let mut app = app_basica();
    app.active_profile = Some(std::ffi::OsString::from("work"));
    app.apply_session(&body);
    let vuelta = app.session_body();

    assert_eq!(
        vuelta.layouts.get("photos"),
        Some(&ajena),
        "\"photos\"'s is untouched"
    );
    assert!(
        vuelta.layouts.contains_key("work"),
        "and this process's goes under ITS name, not under `default`: {:?}",
        vuelta.layouts.keys().collect::<Vec<_>>()
    );
    assert_eq!(vuelta.active, "work");
}

/// ADR 0139: the terminal does not overwrite the WINDOW's layout when
/// writing its own — each remembers its own sizes — and on a HANDOFF it
/// hands the window its own too, under the window's key.
#[test]
fn la_disposicion_de_la_ventana_se_respeta_y_el_relevo_la_entrega() {
    let de_la_ventana = norte_frontend::layout::presets::tree("krusader").expect("preset");
    let mut body = norte_frontend::session::SessionBody::default();
    body.layouts
        .insert("default@window".to_owned(), de_la_ventana.clone());
    let mut app = app_basica();
    app.apply_session(&body);
    assert_eq!(
        app.session_body().layouts.get("default@window"),
        Some(&de_la_ventana),
        "writing its own does not touch the window's"
    );
    let relevo = app.session_body_for_handoff();
    assert_eq!(
        relevo.layouts.get("default@window"),
        relevo.layouts.get("default"),
        "the handoff hands THIS screen to the window"
    );
}

/// The STICKY profile arrives with the session and requests the switch.
///
/// It cannot be applied earlier: it lives in the session, the daemon holds
/// the session, and the daemon is reached with the config that is already
/// loaded. So it is requested and the loop does it through the same path as
/// any other change.
#[test]
fn el_perfil_pegajoso_pide_el_cambio() {
    let body = norte_frontend::session::SessionBody {
        active: "work".to_owned(),
        ..norte_frontend::session::SessionBody::default()
    };
    let mut app = app_basica();
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    assert_eq!(
        app.pending_profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
}

/// And `--profile` WINS: the reader named one for this time, so the one
/// that came from the session does not override it.
#[test]
fn un_perfil_explicito_gana_al_pegajoso() {
    let body = norte_frontend::session::SessionBody {
        active: "photos".to_owned(),
        ..norte_frontend::session::SessionBody::default()
    };
    let mut app = app_basica();
    app.active_profile = Some(std::ffi::OsString::from("work"));
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    assert_eq!(app.pending_profile, None, "no change is requested");
    assert_eq!(
        app.active_profile.as_deref(),
        Some(std::ffi::OsStr::new("work"))
    );
}

/// `ntc <DIR>` wins over the saved session on the active pane, and ONLY
/// there: the rest of the screen comes back as it was. It used to be that
/// the session overrode the argument and `ntc ~/proyecto` opened where it
/// was last closed.
#[test]
fn un_directorio_explicito_gana_a_la_sesion_en_el_panel_activo() {
    let mut app = app_basica();
    let foco = app.focus();
    let activo = app.panes.slot_of(foco);
    let mut spec = app.panes[foco].sort();
    spec.dirs_first = !spec.dirs_first;
    app.panes[foco].set_sort(spec.clone());
    let body = app.session_body();

    let mut other = app_basica();
    let ask = other.apply_session(&body);
    other.pin_start_dir(vp("file:///pedido"));
    let foco = other.focus();
    assert_eq!(other.panes[foco].dir(), &vp("file:///pedido"));
    assert_eq!(
        other.panes[foco].sort(),
        spec,
        "the saved order is kept: only the directory changes"
    );
    assert_eq!(
        other.panes[1 - foco].dir(),
        &vp("file:///der"),
        "the other panel is the session's"
    );
    assert!(ask.contains(&activo), "the slot still requests its listing");
}

/// With no profile, the key is still `default`: whoever never picks one
/// reads and writes exactly where they already did.
#[test]
fn sin_perfil_la_clave_sigue_siendo_default() {
    let app = app_basica();
    let body = app.session_body();
    assert!(body.layouts.contains_key("default"));
    assert_eq!(body.active, "");
}

/// History travels: `nav.back` still works after a restart.
#[test]
fn el_rastro_de_vuelta_sobrevive() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    app.history.for_slot_mut(slot).record(vp("file:///antes"));
    let body = app.session_body();
    assert_eq!(body.slots[&slot.0].back, vec![vp("file:///antes")]);

    let mut other = app_basica();
    other.apply_session(&body);
    assert_eq!(
        other.history.for_slot(slot).expect("history").trail(),
        [vp("file:///antes")]
    );
}

/// Restoring a session does NOT turn off the `..` row.
///
/// The bug this closes: `apply_session` replaced the whole pane and put
/// back only the order and the hidden flag, so `[ui] parent_entry = true`
/// silently turned into `false` from the first saved session onward. The
/// reader saw it as "the TUI has no parent row and the window does," which
/// is the same setting saying two things.
#[test]
fn restaurar_una_sesion_no_apaga_la_fila_de_subir() {
    let mut app = app_basica();
    app.set_parent_row(true);
    let slot = app.panes.slot_of(0);
    assert!(
        app.panes.browser(slot).expect("listing").is_parent_row(0),
        "it starts with it"
    );

    let body = app.session_body();
    let mut otra = app_basica();
    otra.set_parent_row(true);
    otra.apply_session(&body);
    assert!(
        otra.panes.browser(slot).expect("listing").is_parent_row(0),
        "and after applying the session, too"
    );
}

/// And a listing born FROM OUTSIDE — the one startup brings for each
/// session slot — gets it through the same door.
///
/// That was the same hole's other half: `restore_slots` replaced the pane
/// with a freshly listed one and gave it back the order and the hidden
/// flag, never the row. A single door to adopt a foreign listing, and both
/// were covered by it.
#[test]
fn un_listado_adoptado_llega_con_la_fila_de_subir() {
    let mut app = app_basica();
    app.set_parent_row(true);
    let slot = app.panes.slot_of(0);
    app.adoptar_pane(slot, Pane::new(vp("file:///izq"), Vec::new()), None, None);
    assert!(app.panes.browser(slot).expect("listing").is_parent_row(0));

    app.set_parent_row(false);
    app.adoptar_pane(slot, Pane::new(vp("file:///izq"), Vec::new()), None, None);
    assert!(
        !app.panes.browser(slot).expect("listing").is_parent_row(0),
        "and off it does not sneak in either"
    );
}

/// Marks do NOT travel: they are the state of an operation, not of a
/// session.
#[test]
fn las_marcas_no_viajan() {
    let mut app = app_basica();
    app.focused_mut().mark_all();
    let v = serde_json::to_string(&app.session_body().to_value()).expect("json");
    assert!(!v.contains("mark"), "{v}");
}

/// A session that mentions a slot this layout does not have breaks
/// nothing: its state is saved and what the layout says gets painted.
#[test]
fn un_hueco_que_el_layout_no_tiene_no_rompe_la_aplicacion() {
    let mut app = app_basica();
    let mut body = app.session_body();
    let uno = body.slots.values().next().expect("one").clone();
    body.slots.insert(99, uno);
    app.apply_session(&body);
    assert!(app.session_body().slots.contains_key(&99), "it is kept");
}

/// An ORPHANED slot — one the store still has and the layout no longer
/// does — is kept as is, not readopted.
///
/// The question "does this layout have the slot?" was asked of the panes
/// store, and orphans stay there: it answered yes, so the slot went through
/// the adoption door — losing the state the session had saved for it —
/// instead of being written back intact. Returning to yesterday's layout
/// has to give the panel back where it was.
#[test]
fn un_hueco_huerfano_del_almacen_se_conserva_en_vez_de_readoptarse() {
    let mut app = app_basica();
    // A single-slot layout: the second one is orphaned in the store.
    let solo = norte_frontend::layout::presets::tree("simple").expect("preset");
    app.set_layout(solo);
    let fuera = SlotId(2);
    assert!(
        app.panes.browser(fuera).is_some(),
        "the store keeps the orphan"
    );
    assert!(
        !app.layout.slot_ids().contains(&fuera),
        "and the layout no longer places it"
    );

    let mut body = app.session_body();
    let mut estado = body.slots.values().next().expect("one").clone();
    estado.path = vp("file:///de-ayer");
    body.slots.insert(fuera.0, estado);
    app.apply_session(&body);

    assert_eq!(
        app.session_body().slots.get(&fuera.0).map(|s| &s.path),
        Some(&vp("file:///de-ayer")),
        "it is written back as is, without going through the pane"
    );
}

/// A corrupt body does not leave a blank screen: it is ignored and the
/// config's layout stays, with a warning. And it KEEPS getting written: what
/// was unreadable is already lost, and never saving again would be worse.
#[test]
fn un_cuerpo_corrupto_deja_la_pantalla_de_la_config() {
    let mut app = app_basica();
    let before = app.layout.clone();
    // Truly malformed: a slot with no `path`, the only field with no
    // default.
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION,
        &serde_json::json!({ "slots": { "1": { "cursor": 3 } } }),
    );
    assert_eq!(app.layout, before);
    assert!(app.message.is_some(), "and it says so");
    assert!(!app.session.detached, "and this window keeps writing");
}

/// The ENVELOPE's version leaves the window detached the same as the
/// inside one does (#247).
///
/// The body carried an undocumented copy of the version, and it was the
/// only one read: a foreign client doing what the contract says — `version`
/// in the envelope, v2 body — reached a reader that saw it absent, took it
/// for 0, swallowed the fields it did not understand and rewrote them lost.
#[test]
fn la_version_del_sobre_tambien_deja_la_ventana_suelta() {
    let mut app = app_basica();
    let before = app.layout.clone();
    // A body with NO copy inside, which is what a client following the
    // documented contract writes.
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION + 1,
        &serde_json::json!({ "layouts": {}, "slots": {} }),
    );
    assert_eq!(app.layout, before, "what cannot be read is not applied");
    assert!(app.session.detached, "and above all it is not overwritten");
    assert!(app.message.is_some(), "and it says so");
}

/// A body from a NEWER version leaves this window DETACHED: it is not
/// read, and above all, it is not written back over. Without this the
/// warning showed and a second later the dump published the config's
/// screen over the new binary's session.
#[test]
fn un_cuerpo_del_futuro_deja_la_ventana_suelta() {
    let mut app = app_basica();
    let before = app.layout.clone();
    app.apply_session_value(
        norte_frontend::session::SCHEMA_VERSION,
        &serde_json::json!({ "version": 999 }),
    );
    assert_eq!(app.layout, before);
    assert!(
        app.session.detached,
        "what cannot be read is not overwritten"
    );
    assert!(app.message.is_some(), "and it says so");
}

/// The two schema versions — the core's, which decides whether a file can
/// be overwritten, and the frontend's, which decides whether a body can be
/// read — go hand in hand.
///
/// They live in different crates because the core cannot depend on the
/// frontend, and nothing ties them at compile time: bumping ONLY the
/// frontend's makes the core refuse its own file on every startup, forever
/// and behind a `warn!`. This is the only crate that sees both.
#[test]
fn las_dos_versiones_de_esquema_van_del_brazo() {
    assert_eq!(
        norte_core::ui_session::disk::SCHEMA_VERSION,
        norte_frontend::session::SCHEMA_VERSION,
        "if you bump one, bump the other"
    );
}

/// The cursor is placed when the listing arrives, not before: on an empty
/// pane row 12 is row 0, and applying it there would lose it.
#[test]
fn el_cursor_espera_a_su_listado() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut body = app.session_body();
    body.slots.get_mut(&slot.0).expect("slot").cursor = 2;
    app.apply_session(&body);
    assert_eq!(
        app.panes[0].cursor(),
        0,
        "with no listing there is nowhere to put it"
    );

    let entradas: Vec<norte_proto::Entry> = ["a", "b", "c", "d"]
        .iter()
        .map(|n| norte_proto::Entry {
            path: vp(&format!("file:///izq/{n}")),
            kind: norte_proto::EntryKind::File,
            size: Some(0),
            mtime_ms: None,
            attrs: std::collections::BTreeMap::new(),
        })
        .collect();
    app.panes[0].begin_listing(vp("file:///izq"), entradas, false, None);
    app.restore_cursor(slot);
    assert_eq!(app.panes[0].cursor(), 2);
}

/// A handoff's MARKS wait for their listing, like the cursor — and only
/// come back with `--attach` (phase 9).
///
/// Both halves came out of piloting it, and the first is a bug a green
/// test would not have seen: seeding them on session apply looked like it
/// worked — the mark set is by path, not by index — but `begin_listing`
/// clears them when the listing arrives, which is correct for a cd and
/// fatal for a seed made too early. The handoff returned the screen with
/// nothing marked, silently.
#[test]
fn las_marcas_de_un_relevo_esperan_a_su_listado_y_solo_con_attach() {
    fn listado(app: &mut norte_tui::app::App) {
        let entradas: Vec<norte_proto::Entry> = ["a", "b", "c"]
            .iter()
            .map(|n| norte_proto::Entry {
                path: vp(&format!("file:///izq/{n}")),
                kind: norte_proto::EntryKind::File,
                size: Some(0),
                mtime_ms: None,
                attrs: std::collections::BTreeMap::new(),
            })
            .collect();
        app.panes[0].begin_listing(vp("file:///izq"), entradas, false, None);
    }

    // WITHOUT `--attach`: an ordinary startup is not a handoff, and
    // yesterday's selection is not revived.
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut body = app.session_body();
    body.slots.get_mut(&slot.0).expect("slot").path = vp("file:///izq");
    body.slots.get_mut(&slot.0).expect("slot").marks = vec![vp("file:///izq/a")];
    app.apply_session(&body);
    listado(&mut app);
    app.restore_cursor(slot);
    assert_eq!(
        app.panes[0].marks_len(),
        0,
        "without --attach a startup does not return what was marked"
    );

    // WITH `--attach`: it comes back, and after the listing.
    let mut app = app_basica();
    app.session.attach = true;
    app.apply_session(&body);
    assert_eq!(
        app.panes[0].marks_len(),
        0,
        "not yet: the pane is empty and the listing would clear them"
    );
    listado(&mut app);
    app.restore_cursor(slot);
    assert_eq!(
        app.panes[0].marks_len(),
        1,
        "and now it does, with its listing"
    );
}

/// A slot whose layout DOES exist gets back its directory and its order.
#[test]
fn el_directorio_y_el_orden_vuelven() {
    let mut app = app_basica();
    let slot = app.panes.slot_of(0);
    let mut spec = app.panes[0].sort();
    spec.dirs_first = !spec.dirs_first;
    app.panes[0].set_sort(spec.clone());
    app.panes[0].set_show_hidden(false);
    let body = app.session_body();

    let mut other = app_basica();
    let ask = other.apply_session(&body);
    assert!(ask.contains(&slot), "it needs listing");
    assert_eq!(other.panes[0].dir(), &vp("file:///izq"));
    assert_eq!(other.panes[0].sort(), spec);
    assert!(!other.panes[0].show_hidden());
}

/// `[profile.start]` opens the slot the session knows nothing about.
///
/// It is what makes a freshly created profile useful, or one arriving from
/// another machine. Both frontends wrote the key and NEITHER read it, with
/// two files promising they did.
#[test]
fn profile_start_siembra_un_hueco_sin_sesion() {
    let mut app = app_basica();
    let body = norte_frontend::session::SessionBody::default();
    // Through `apply_session_value`, the real path: it is where the
    // terminal notes which slots it KNOWS were saved, and that note is the
    // veto. Calling the seeder with a hand-built body would only test the
    // harness.
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);

    let sembrados = app.seed_profile_start(&start);

    assert_eq!(sembrados, vec![SlotId(1)]);
    assert_eq!(
        app.panes
            .browser(SlotId(1))
            .expect("there is a listing")
            .dir(),
        &vp("file:///fotos")
    );
}

/// And only the FIRST time. With no saved session — a fresh install — the
/// session never knows anything about any slot, so without keeping count of
/// what was seeded, entering and leaving the profile pulled the reader away
/// from where they were on every round: ADR 0098's decision 2, upside down.
#[test]
fn un_hueco_ya_sembrado_no_se_vuelve_a_sembrar() {
    let mut app = app_basica();
    let body = norte_frontend::session::SessionBody::default();
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);
    assert_eq!(app.seed_profile_start(&start), vec![SlotId(1)]);

    // The reader goes somewhere else and re-enters the profile.
    app.adoptar_pane(
        SlotId(1),
        Pane::new(vp("file:///trabajo"), Vec::new()),
        None,
        None,
    );

    assert!(app.seed_profile_start(&start).is_empty());
    assert_eq!(
        app.panes
            .browser(SlotId(1))
            .expect("there is a listing")
            .dir(),
        &vp("file:///trabajo"),
        "seeding is a first-time-only thing"
    );
}

/// And the SESSION wins: `[profile.start]` says where a slot opens the
/// first time, not every time.
///
/// A profile is a workspace, not a bookmark that pulls you back to the
/// start: if every entry pulled you away from where you were, it would be
/// useless for exactly the people who use it daily.
#[test]
fn la_sesion_gana_a_profile_start() {
    let mut app = app_basica();
    let mut body = norte_frontend::session::SessionBody::default();
    body.slots.insert(
        1,
        norte_frontend::session::SlotState {
            path: vp("file:///donde/lo/dejaste"),
            cursor: 0,
            back: Vec::new(),
            forward: Vec::new(),
            jump: None,
            sort: norte_frontend::SortSpec::default(),
            columns: Vec::new(),
            show_hidden: false,
            touched_ms: 0,
            marks: Vec::new(),
        },
    );
    app.apply_session_value(norte_frontend::session::SCHEMA_VERSION, &body.to_value());
    let start = std::collections::BTreeMap::from([(1, vp("file:///fotos"))]);

    assert!(
        app.seed_profile_start(&start).is_empty(),
        "a slot the session already places is not seeded"
    );
    assert_eq!(
        app.panes
            .browser(SlotId(1))
            .expect("there is a listing")
            .dir(),
        &vp("file:///donde/lo/dejaste")
    );
}

/// An id the profile names and this layout does not place has nowhere to
/// open: it falls silently instead of minting a slot nobody asked for.
#[test]
fn profile_start_no_inventa_un_hueco_que_el_layout_no_tiene() {
    let mut app = app_basica();
    let start = std::collections::BTreeMap::from([(4242, vp("file:///fotos"))]);
    assert!(app.seed_profile_start(&start).is_empty());
    assert!(app.panes.browser(SlotId(4242)).is_none());
}

/// And it does not overwrite an ORPHANED slot either: one the store keeps
/// and this layout does not place.
///
/// The store cannot be asked "does it exist?": it keeps orphans for when
/// its layout comes back, and `insert` REVIVES them. Seeding there, the
/// reader returns to that layout and finds the panel in the profile's
/// startup directory instead of where they left it — which is the promise
/// the store has written down. The LAYOUT is asked instead, the same fix
/// `apply_session` documents thirty lines up.
#[test]
fn profile_start_no_revive_un_hueco_huerfano() {
    let mut app = app_basica();
    // Slot 3 exists in the store and NOT in the layout.
    app.set_layout(Node::split(
        norte_frontend::layout::Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(3), KindId::browser()),
        ],
    ));
    app.adoptar_pane(
        SlotId(3),
        Pane::new(vp("file:///lo/de/ayer"), Vec::new()),
        None,
        None,
    );
    app.set_layout(Node::slot(SlotId(1), KindId::browser()));

    let start = std::collections::BTreeMap::from([(3, vp("file:///fotos"))]);
    assert!(app.seed_profile_start(&start).is_empty());
    assert_eq!(
        app.panes.browser(SlotId(3)).map(Pane::dir),
        Some(&vp("file:///lo/de/ayer")),
        "the orphan stays where it was, for when its layout comes back"
    );
}

/// A layout this binary does not know how to paint still travels: the
/// session saves the tree, it does not interpret it.
#[test]
fn un_kind_desconocido_viaja_en_el_layout() {
    let mut app = app_basica();
    app.set_layout(Node::split(
        norte_frontend::layout::Dir::Vertical,
        vec![
            Node::slot(SlotId(1), KindId::browser()),
            Node::slot(SlotId(2), KindId::new("kind-de-otro-binario")),
        ],
    ));
    let body = app.session_body();
    let vuelta = norte_frontend::session::SessionBody::from_value(
        norte_frontend::session::SCHEMA_VERSION,
        &body.to_value(),
    )
    .expect("parses");
    assert_eq!(vuelta.layouts["default"], app.layout);
}
