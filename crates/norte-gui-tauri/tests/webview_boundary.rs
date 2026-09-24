//! The webview's boundary, pinned (ADR 0066, decision D11).
//!
//! These checks read the CONFIGURATION and the packaged assets, not the
//! runtime, and that is exactly the point: a relaxed CSP, one capability too
//! many, or a `<script src="https://…">` slipped into the bundle are not
//! seen in any behavior test — they are seen here, in the diff, or never.

use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn json(rel: &str) -> serde_json::Value {
    let p = root().join(rel);
    let raw = std::fs::read_to_string(&p)
        .unwrap_or_else(|e| panic!("could not read {}: {e}", p.display()));
    serde_json::from_str(&raw).expect("valid JSON")
}

/// The CSP leaves no gap: no remote scripts, no `eval`, no inline styles.
#[test]
fn la_csp_no_deja_puertas() {
    let cfg = json("tauri.conf.json");
    let csp = cfg["app"]["security"]["csp"]
        .as_str()
        .expect("the CSP is set");
    // The ONLY origin with a scheme that is allowed is Tauri's own IPC,
    // which is not remote: it is how the webview talks to this process.
    // Everything else — a CDN, a dev websocket, a wildcard — is a door to
    // the outside and cannot be there.
    let sin_ipc = csp.replace("http://ipc.localhost", "");
    for prohibido in [
        "'unsafe-inline'",
        "'unsafe-eval'",
        "http://",
        "https://",
        "ws://",
        "wss://",
        "*",
    ] {
        assert!(
            !sin_ipc.contains(prohibido),
            "the CSP cannot contain {prohibido}: {csp}"
        );
    }
    assert!(
        csp.starts_with("default-src 'none'"),
        "closed by default: {csp}"
    );
    for directiva in [
        "script-src 'self'",
        "object-src 'none'",
        "base-uri 'none'",
        "frame-ancestors 'none'",
    ] {
        assert!(csp.contains(directiva), "missing `{directiva}`: {csp}");
    }
    // Images: `blob:` YES, `data:` NO (ADR 0069).
    //
    // `blob:` cannot be manufactured from content — a blob URL exists only
    // because this document created it — so it is a narrower concession
    // than `data:`, which is a URL any string can form. The difference
    // matters even though this document does not paint foreign markup
    // today: the CSP belongs to the WHOLE DOCUMENT, not the element we had
    // in mind.
    assert!(
        csp.contains("img-src 'self' blob:"),
        "images cross as a blob (ADR 0069): {csp}"
    );
    assert!(
        !csp.contains("data:"),
        "`data:` does not enter the CSP without changing ADR 0069, which \
         explains why `blob:` was chosen: {csp}"
    );
}

/// The webview has neither Tauri's global object, nor an asset protocol, nor
/// a dev server to reach in production.
#[test]
fn the_window_brings_nothing_stock() {
    let cfg = json("tauri.conf.json");
    assert_eq!(
        cfg["app"]["withGlobalTauri"],
        serde_json::Value::Bool(false),
        "no `window.__TAURI__`: what can be called is imported, and it is on the list"
    );
    assert_eq!(
        cfg["app"]["security"]["assetProtocol"]["enable"],
        serde_json::Value::Bool(false),
        "with no asset protocol there is no way to ask it for a file from disk"
    );
    assert!(
        cfg["build"]["devUrl"].is_null(),
        "a production binary does not point at a dev server"
    );
    // Dropping DOES mean something since #283 (ADR 0074), and what it means
    // is a question: the drop reaches the process — never the webview, which
    // does not see the paths — and opens the copy confirmation. The
    // assertion stays because the value is a decision, not an oversight: if
    // someone sets it back to `false` they will have erased the whole
    // gesture without touching a line of Rust.
    assert_eq!(
        cfg["app"]["windows"][0]["dragDropEnabled"],
        serde_json::Value::Bool(true),
        "dropping enters through the process and opens a confirmation (#283)"
    );
}

/// The capabilities file grants the BARE MINIMUM: listening for events.
/// Nothing about filesystem, shell, http, native dialog or window control.
#[test]
fn the_capabilities_are_the_minimum() {
    let cap = json("capabilities/main.json");
    let permissions: Vec<&str> = cap["permissions"]
        .as_array()
        .expect("there is a permission list")
        .iter()
        .map(|p| p.as_str().expect("each permission is a string"))
        .collect();
    assert_eq!(
        permissions,
        vec!["core:event:allow-listen", "core:event:allow-unlisten"],
        "any extra permission is a decision, and it shows up here"
    );
    assert_eq!(
        cap["windows"].as_array().map(Vec::len),
        Some(1),
        "one window, the main one"
    );
}

/// The commands the binary registers are EXACTLY the declared ones.
///
/// `main.rs` is read on purpose: `generate_handler!` is a macro, so a new
/// command does not show up in any list that can be compared at compile
/// time. This turns it into something that breaks the test.
#[test]
fn the_command_surface_is_the_declared_one() {
    let src = std::fs::read_to_string(root().join("src/main.rs")).expect("main.rs");
    // The PRODUCTION block, which is `not(feature = "metrics")`'s: 3.6's
    // instrumentation adds one more command and cannot sneak in here.
    let (_, after_cfg) = src
        .split_once("#[cfg(not(feature = \"metrics\"))]")
        .expect("the production handler is marked");
    let (_, rest) = after_cfg
        .split_once("generate_handler![")
        .expect("the binary registers commands");
    let (block, _) = rest.split_once(']').expect("the macro closes");
    let registered: Vec<String> = block
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    assert_eq!(
        registered,
        norte_gui_tauri::commands::COMMANDS,
        "the declared list and the registered one have to be the same"
    );
}

/// The production bundle brings in nothing remote, no `eval`, no dev-server
/// client.
///
/// If there is no bundle yet, the test SAYS SO and does not look past it: a
/// "there was nothing to look at" that reads as green is worse than a red.
#[test]
fn the_bundle_does_not_phone_home() {
    let dist = root().join("ui/dist");
    let index = dist.join("index.html");
    assert!(
        index.exists(),
        "no bundle at {}: run `just gui-build` first. A \"there was nothing \
         to look at\" that reads as green is worse than a red — and this is \
         what its own comment used to say while doing the opposite.",
        dist.display()
    );
    let mut mirados = 0usize;
    for entry in walk(&dist) {
        let Some(ext) = entry.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !matches!(ext, "html" | "js" | "css") {
            continue;
        }
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        // The ONLY exception, and an exact one: the SVG namespace
        // `createElementNS` needs for the activity bar's icons (ADR 0131).
        // It is an XML identifier, not an address: the browser never
        // requests it. The whole string is removed before looking, so
        // `http://www.w3.org/something-else` is still red.
        let text = text.replace("http://www.w3.org/2000/svg", "");
        mirados += 1;
        for prohibido in [
            "http://",
            "https://",
            "ws://",
            "wss://",
            "eval(",
            "new Function(",
        ] {
            assert!(
                !text.contains(prohibido),
                "{} contains `{prohibido}`",
                entry.display()
            );
        }
    }
    assert!(mirados >= 2, "the HTML and its script were both looked at");
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(dir) else {
        return out;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}

/// The webview does not navigate outside its own assets.
///
/// Two of task 3.3's acceptance points ("navigating to
/// `https://example.invalid` is rejected", "`window.open` does not create an
/// unrestricted webview") had NEITHER an implementation NOR a test: the CSP
/// does not cover top-level navigation. This pins the guard that does.
#[test]
fn the_webview_does_not_navigate_anywhere() {
    let src = std::fs::read_to_string(root().join("src/main.rs")).expect("main.rs");
    assert!(
        src.contains("navigation_guard"),
        "the binary has to install the navigation guard"
    );
    assert!(
        src.contains(r#"const PAGE_SCHEMAS: &[&str] = &["tauri", "ipc"];"#),
        "and the scheme list is EXACTLY that: any addition is a decision \
         that shows up in the diff"
    );
}

/// `window_control` (ADR 0136) is the webview's ONLY door to its window, and
/// its shape is the decision: four verbs, rejection with the native bar
/// before touching anything, and closing with `close()` — which goes through
/// `CloseRequested` and therefore through `[ui] confirm_quit` — and never
/// with `destroy()`, which would skip the question and the session save.
#[test]
fn the_windows_door_is_narrow() {
    use norte_gui_tauri::commands::WindowVerb;
    for (text, verb) in [
        ("\"minimize\"", WindowVerb::Minimize),
        ("\"toggle_maximize\"", WindowVerb::ToggleMaximize),
        ("\"close\"", WindowVerb::Close),
        ("\"drag\"", WindowVerb::Drag),
    ] {
        assert_eq!(serde_json::from_str::<WindowVerb>(text).ok(), Some(verb));
    }
    for foreign in [
        "\"destroy\"",
        "\"set_position\"",
        "\"set_size\"",
        "\"hide\"",
    ] {
        assert!(
            serde_json::from_str::<WindowVerb>(foreign).is_err(),
            "{foreign} is not a title-bar verb"
        );
    }
    let src = std::fs::read_to_string(root().join("src/main.rs")).expect("main.rs");
    let (_, body) = src
        .split_once("fn window_control(")
        .expect("the binary declares `window_control`");
    let (body, _) = body.split_once("\n}\n").expect("the function closes");
    let rejection = body
        .find("custom_titlebar")
        .expect("checks whether the bar is the custom one");
    let primer_verb = body.find("match verb").expect("dispatches by verb");
    assert!(
        rejection < primer_verb,
        "rejects with the native bar BEFORE touching the window"
    );
    assert!(body.contains("window.close()"), "closes with `close()`");
    assert!(
        !body.contains("destroy"),
        "`destroy()` skips `confirm_quit` and the session save"
    );
}

/// 3.6's instrumentation does not travel in the binary by default.
///
/// The test that pins the command list reads `main.rs`, so it would pass
/// just the same with `default = ["metrics"]` in the manifest: the fifth
/// command would enter through the features door and no test would see it.
#[test]
fn the_measurement_feature_is_not_the_default_one() {
    let toml = std::fs::read_to_string(root().join("Cargo.toml")).expect("Cargo.toml");
    let features = toml
        .split_once("[features]")
        .map(|(_, rest)| rest.split("\n[").next().unwrap_or_default().to_owned())
        .unwrap_or_default();
    assert!(
        features.contains("metrics"),
        "the feature exists and is declared here"
    );
    assert!(
        !features.contains("default"),
        "and there is NO `default`: measuring is asked for by hand or it is not there"
    );
}

/// The renderer does not call a command the binary does not expose.
///
/// The SOURCE is checked, not the bundle: the bundler minifies the call
/// (`t(`dispatch`)`), so in `dist` the name is no longer glued to `invoke(`
/// and any sweep there is guessing. In `ui/src` it is, and that is where
/// someone would add a new command.
#[test]
fn the_renderer_only_invokes_known_commands() {
    let src = root().join("ui/src");
    let conocidos: Vec<&str> = norte_gui_tauri::commands::COMMANDS
        .iter()
        .copied()
        // `metrics` only exists with its feature; the renderer calls it
        // unconditionally and the production binary rejects it.
        .chain(std::iter::once("metrics"))
        .collect();
    let mut vistos = Vec::new();
    for entry in walk(&src) {
        if entry.extension().and_then(|e| e.to_str()) != Some("ts") {
            continue;
        }
        let text = std::fs::read_to_string(&entry).unwrap_or_default();
        for chunk in text.split("invoke").skip(1) {
            // `invoke<T>("name"` or `invoke("name"`.
            let Some(opens) = chunk.find('(') else {
                continue;
            };
            let rest = &chunk[opens + 1..];
            let Some(name) = rest
                .trim_start()
                .strip_prefix('"')
                .and_then(|r| r.split('"').next())
            else {
                continue;
            };
            vistos.push(name.to_owned());
        }
    }
    assert!(!vistos.is_empty(), "the renderer calls something");
    for n in &vistos {
        assert!(
            conocidos.contains(&n.as_str()),
            "the renderer calls `{n}`, which is not in the declared surface"
        );
    }
    // And all four production ones are used: a declared surface nobody
    // calls is a surface nobody maintains.
    for c in norte_gui_tauri::commands::COMMANDS {
        assert!(
            vistos.iter().any(|v| v == c),
            "nobody calls `{c}`: is it extra in the list?"
        );
    }
}

/// The ORDER of the document's anchors decides who covers whom.
///
/// This screen does not use `z-index` anywhere: among positioned elements the
/// document order rules. So the order IS the decision, and until now it was
/// only written in comments in the stylesheet and in the HTML itself.
///
/// The menu uncovered it: declared BEFORE `#screen`, its dropdown ended up
/// underneath the panes — which are absolute and come after — and opened
/// invisible. It shows when clicked and shows in no behavior test, because
/// `jsdom` does not lay out a screen.
#[test]
fn the_order_of_the_anchors_is_who_covers_whom() {
    let html = std::fs::read_to_string(root().join("ui/index.html")).expect("the index is there");
    let pos = |id: &str| {
        html.find(&format!("id=\"{id}\""))
            .unwrap_or_else(|| panic!("missing anchor #{id}"))
    };
    assert!(
        pos("screen") < pos("menu"),
        "the menu goes AFTER the screen: its dropdown hangs over the panes, \
         and whoever comes first ends up underneath"
    );
    // And the surfaces that grab the keyboard go after the menu: a dialog or
    // help rule over a menu bar, never the other way around.
    for above in ["palette", "dialogs", "help", "profiles"] {
        assert!(
            pos("menu") < pos(above),
            "#{above} has to cover the menu, so it goes after"
        );
    }
}

/// The window ALREADY mutates, and it is still a barrier pinned here.
///
/// The switch was flipped by task 5.4 (the mutation security review phase
/// 5's exit gate requires), and this test changed at the same time: while it
/// was `SoloRead` (read-only), the whole promise rested on a constant no
/// test looked at, and changing it by accident left the whole suite green
/// and the window deleting files.
///
/// It stays here in the other direction too: going back to `SoloRead`
/// also has to be a decision, not a merge. The constant's rustdoc says what
/// backs it.
#[test]
fn the_window_mutates_and_it_is_a_decision() {
    assert_eq!(
        norte_gui_tauri::startup::EFFECTS,
        norte_ui_host::commands::Effects::Full,
        "changing the effects switch is a 5.4 decision, not an oversight"
    );
}
