//! Every catalogue method REACHES the surfaces it should (ADR 0089).
//!
//! The catalogue (`norte_proto::catalog`) says which methods exist. Here each
//! surface is asked whether it has it: the daemon's dispatch, the remote
//! client, and the schema's aggregate.
//!
//! It lives in `norte-core` and not in `norte-proto` because this is where
//! the three files can be read. They are read as TEXT on purpose: checking it
//! with types would require the daemon's flat dispatch to stop being flat,
//! and that flatness is deliberate — a hundred-arm `match` where each arm
//! reads whole is better than ten layers you have to walk through to know
//! what `fs.stat` does. What was missing was not structure, it was that
//! forgetting one would show.
//!
//! What this test CANNOT say is whether the arm does the right thing. It says
//! it exists. That is exactly the class of oversight that was slipping
//! through.

use norte_proto::catalog::{CATALOG, Kind, MethodInfo, Shape};

/// Reads a workspace file from the crate's root.
fn source(rel: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

/// The constant's name from the wire one (`fs.stat` → `FS_STAT`).
///
/// The CONSTANT is looked up, not the string: the daemon and the client name
/// `methods::FS_STAT`, never the bare `"fs.stat"`, and looking up the string
/// would give false negatives across the board.
fn constant_of(m: &MethodInfo) -> String {
    m.name.replace('.', "_").to_uppercase()
}

/// Does `methods::K` appear as a WHOLE identifier?
///
/// Not with `contains`, which was the bug: `methods::SYNC_PLAN` is a
/// substring of `methods::SYNC_PLAN_DONE`, so deleting `sync.plan`'s arm left
/// the test green. It happened to about fifteen methods — all the ones with a
/// longer sibling constant: `FS_CHECKSUM`/`_REPORT`,
/// `PLUGIN_PREVIEW`/`_STYLED`, `FS_READ`/`FS_READ_MAX_CHUNK`… — meaning the
/// check was indicative and not falsifiable exactly where it mattered most.
fn names(src: &str, k: &str) -> bool {
    let needle = format!("methods::{k}");
    src.match_indices(&needle).any(|(i, _)| {
        let next = src[i + needle.len()..].chars().next();
        !next.is_some_and(|c| c.is_alphanumeric() || c == '_')
    })
}

/// Does it have a dispatch ARM (`methods::K =>`)?
///
/// This is what tells a request apart from a notification in the daemon, and
/// what stops the name from counting just because it appears in a doc comment
/// or in the list of cancelables. `RPC_CANCEL` was catalogued as a request
/// and has no arm: with this check it would have gone red on day one.
fn has_arm(src: &str, k: &str) -> bool {
    let needle = format!("methods::{k}");
    src.match_indices(&needle).any(|(i, _)| {
        let rest = src[i + needle.len()..].trim_start();
        rest.starts_with("=>")
    })
}

/// The text of `k`'s dispatch ARM: from `methods::K =>` to the start of the
/// next arm.
///
/// Slicing by arms is what makes `shape` falsifiable. Without this it was the
/// only catalogue field nobody checked — and it lied on day one: `index.build`
/// was declared `Direct` with `IndexBuildResult` when the daemon registers a
/// Task and answers `FsTaskResult`.
fn arm_of<'a>(src: &'a str, k: &str) -> Option<&'a str> {
    let needle = format!("methods::{k}");
    let start = src
        .match_indices(&needle)
        .find(|(i, _)| src[i + needle.len()..].trim_start().starts_with("=>"))?
        .0;
    let rest = &src[start + needle.len()..];
    // The arm ends at the FIRST of these three: the next `methods::… =>`, the
    // wildcard arm, or the function's end. Without the last two, the last arm
    // of every `match` swallowed the rest of the file and dragged in the
    // `FsTaskResult` of any function further down — which is how
    // `connection.provide_secret` and `plugin.set_config` came out marked
    // without registering anything.
    let next_arm = rest.match_indices("methods::").find(|(i, _)| {
        let after = &rest[*i + "methods::".len()..];
        let ident: String = after
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        !ident.is_empty()
            && rest[*i + "methods::".len() + ident.len()..]
                .trim_start()
                .starts_with("=>")
    });
    let end = [
        next_arm.map(|(i, _)| i),
        rest.find("other =>"),
        rest.find("\n}"),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(rest.len());
    Some(&rest[..end])
}

/// The body of the function an arm DELEGATES to, if it delegates.
///
/// Almost every arm is a single line calling a `handle_…`, so looking only at
/// the arm does not say whether it registers a Task. Without following the
/// delegation, the check below can only fail in one direction — which is how
/// the first version was left.
fn delegated_body<'a>(src: &'a str, arm: &str) -> Option<&'a str> {
    let name: String = arm
        .match_indices("handle_")
        .chain(arm.match_indices("dispatch_"))
        .map(|(i, _)| {
            arm[i..]
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect::<String>()
        })
        .next()?;
    for prefix in ["async fn ", "fn "] {
        let needle = format!("{prefix}{name}");
        if let Some(i) = src.find(&needle) {
            let rest = &src[i..];
            // Up to the next top-level item.
            let end = rest[1..].find("\n}\n").map_or(rest.len(), |j| j + 1 + 3);
            return Some(&rest[..end]);
        }
    }
    None
}

/// **A `Task` or `Stream` method registers a Task; a `Direct` one does not.**
///
/// This is the check that makes `shape` something that can be disproven, and
/// it disproves it in BOTH directions: declaring `Direct` and registering a
/// Task, and declaring `Task` without registering any. The first version only
/// looked at the former, so `shape` could still lie with the gate green —
/// which is exactly what this catalogue exists to prevent.
#[test]
fn the_catalogues_shape_matches_what_the_daemon_does() {
    let src = source("src/daemon/server.rs");
    // `register_task` and `FsTaskResult` are the two ways a handler returns a
    // `task_id`. A bare `task_id` does NOT count: there are direct methods
    // that RECEIVE it as a parameter (`task.cancel`, the reports).
    let registers = |t: &str| t.contains("FsTaskResult") || t.contains("register_task");

    let mut bad = Vec::new();
    for m in CATALOG.iter().filter(|m| m.kind == Kind::Request) {
        let Some(arm) = arm_of(&src, &constant_of(m)) else {
            continue; // Covered by `the_daemon_dispatches_every_request`.
        };
        let delegate = delegated_body(&src, arm);
        let is_task = registers(arm) || delegate.is_some_and(registers);
        let declared_task = matches!(m.shape, Shape::Task | Shape::Stream);

        match (is_task, declared_task) {
            (true, false) => bad.push(format!("{} says Direct and registers a Task", m.name)),
            (false, true) => bad.push(format!(
                "{} says {:?} and registers no Task",
                m.name, m.shape
            )),
            _ => {}
        }
    }
    assert!(
        bad.is_empty(),
        "the catalogue does not say what the daemon does: {bad:?}"
    );
}

/// The methods a surface has no reason to name, with its reason.
///
/// A list of exceptions is debt: each entry says WHY something is not
/// checked, and without a written reason it does not go in.
struct Exception {
    method: &'static str,
    reason: &'static str,
}

/// **The daemon's dispatch names every request.**
#[test]
fn the_daemon_dispatches_every_request() {
    let src = source("src/daemon/server.rs");
    // Notifications are not dispatched: the daemon EMITS them, and they also
    // appear in this file, so they are checked the same way further below.
    let mut missing = Vec::new();
    for m in CATALOG.iter().filter(|m| m.kind == Kind::Request) {
        if !has_arm(&src, &constant_of(m)) {
            missing.push(m.name);
        }
    }
    assert!(
        missing.is_empty(),
        "requests the daemon does not dispatch: {missing:?}.\n\
         A declared method the daemon does not serve answers \
         METHOD_NOT_FOUND to a client that believes it is supported."
    );
}

/// **And a notification does NOT have a dispatch arm.**
///
/// The other half, the one that ties `kind` down instead of leaving it to my
/// word: if something catalogued as a notification were dispatched as a
/// request, or the other way around, one of the two checks falls. This is
/// what caught `rpc.cancel` being catalogued as a request.
#[test]
fn a_notification_is_not_dispatched_as_a_request() {
    let src = source("src/daemon/server.rs");
    let mut extra = Vec::new();
    for m in CATALOG.iter().filter(|m| m.kind == Kind::Notification) {
        if has_arm(&src, &constant_of(m)) {
            extra.push(m.name);
        }
    }
    assert!(
        extra.is_empty(),
        "catalogued as a notification but the daemon dispatches them as a \
         request: {extra:?}. One of the two things is a lie."
    );
}

/// **The daemon emits every notification the catalogue declares.**
#[test]
fn the_daemon_emits_every_notification() {
    let src = [
        source("src/daemon/server.rs"),
        source("src/daemon/mod.rs"),
        source("src/engine.rs"),
        // `rpc.cancel` is sent by the CLIENT when dropping a request, not by
        // the daemon: it is the only notification that goes in that direction.
        source("../norte-client/src/remote/calls.rs"),
    ]
    .join("\n");
    let mut missing = Vec::new();
    for m in CATALOG.iter().filter(|m| m.kind == Kind::Notification) {
        if !names(&src, &constant_of(m)) {
            missing.push(m.name);
        }
    }
    assert!(
        missing.is_empty(),
        "notifications nobody emits: {missing:?}.\n\
         A declared notification that is never sent is a screen waiting for \
         something that will never arrive."
    );
}

/// **The remote client knows how to request everything the catalogue
/// declares.**
///
/// This is the surface that is most silently forgotten: the daemon serves the
/// method, the schema publishes it, and the window has no way to call it.
#[test]
fn the_remote_client_knows_how_to_ask_for_everything() {
    // The FOUR files of the remote client. Reading only two made the
    // stale-exceptions check lie: it claimed the client did not request
    // `rpc.cancel` when it sends it in `calls.rs`.
    let src = [
        source("../norte-client/src/remote/mod.rs"),
        source("../norte-client/src/remote/paging.rs"),
        source("../norte-client/src/remote/calls.rs"),
        source("../norte-client/src/remote/routes.rs"),
    ]
    .join("\n");

    // The client is NOT the daemon: some methods by design are not its concern.
    let exceptions = [
        Exception {
            method: "daemon.shutdown",
            reason: "shutting down the daemon is an act of the CLI, not of the SDK that uses it",
        },
        Exception {
            method: "policy.request_scope",
            reason: "an AGENT asks for it over MCP, not a window",
        },
        Exception {
            // This test uncovered it on its first pass, and it turned out not
            // to be an oversight: it is granted from the TERMINAL
            // (`norte policy grant`, `norte-cli/src/main.rs`), with the
            // daemon's low-level client and not the SDK. That the gap existed
            // on purpose was not written anywhere; now it is.
            method: "policy.grant_scope",
            reason: "granting a scope to an agent is a deliberate act of the \
                     CLI; no window offers it",
        },
    ];

    let mut missing = Vec::new();
    for m in CATALOG.iter().filter(|m| m.kind == Kind::Request) {
        if exceptions.iter().any(|e| e.method == m.name) {
            continue;
        }
        if !names(&src, &constant_of(m)) {
            missing.push(m.name);
        }
    }
    assert!(
        missing.is_empty(),
        "requests the remote client does not know how to make: {missing:?}.\n\
         If it is on purpose, it goes into `exceptions` WITH its reason; if \
         not, it is a method the daemon serves that no frontend can ask for."
    );

    // An exception that is no longer needed is debt that lingers: if the
    // client learned to ask for it, it comes off the list.
    for e in &exceptions {
        let Some(m) = norte_proto::catalog::search(e.method) else {
            panic!(
                "the exception `{}` names a method that does not exist",
                e.method
            );
        };
        assert!(
            !names(&src, &constant_of(m)),
            "`{}` is exempted ({}) but the client DOES request it: remove the exception",
            e.method,
            e.reason
        );
    }
}

/// **The catalogue's types are in the schema's aggregate.**
///
/// The published schema is generated from a hand-written `struct` with one
/// field per wire type. A new method whose `Params` does not go in there is
/// left out of the published schema without anything going red.
#[test]
fn the_catalogues_types_are_in_the_schema() {
    let src = source("../norte-proto/tests/schema.rs");
    let mut missing = Vec::new();
    for m in CATALOG {
        for ty in [m.params(), m.result()].into_iter().flatten() {
            // `methods::FsStatParams` → `FsStatParams`, which is how the
            // aggregate names it (with or without the module prefix).
            let short = ty.rsplit("::").next().unwrap_or(ty).trim();
            if !src.contains(short) {
                missing.push(format!("{} → {short}", m.name));
            }
        }
    }
    missing.sort();
    missing.dedup();
    assert!(
        missing.is_empty(),
        "catalogue types not in the schema's aggregate: {missing:?}.\n\
         What is not in `ProtocolSchema` is not published, and whoever \
         implements the protocol from the schema will not know it exists."
    );
}
