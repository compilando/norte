# Architecture decision records

ADRs use the MADR structure and are created with the `/adr` project command.
The specification evolves through explicit decisions rather than undocumented
edits.

| No. | Decision | Status |
| --- | --- | --- |
| [0001](0001-vpath-wire-format.md) | VPath representation and wire format | accepted |
| [0002](0002-async-runtime-blocking-io.md) | Async runtime and blocking I/O | accepted |
| [0003](0003-workspace-lints-licenses.md) | Workspace structure, lint policy, and crate licenses | accepted |
| [0004](0004-protocol-wire-conventions.md) | Protocol v0 wire conventions and evolution | accepted |
| [0005](0005-provider-contract-copy-policies.md) | Provider contract expansion and copy policies | accepted |
| [0006](0006-keymap-resolution.md) | Keymap resolution semantics | accepted |
| [0007](0007-layered-config-hot-reload.md) | Layered configuration and hot reload | accepted |
| [0008](0008-norte-encoding-boundary.md) | The norte-encoding boundary | accepted |
| [0009](0009-trash.md) | Trash support and explicit permanent-delete fallback | accepted |
| [0010](0010-core-plugin-config-boundaries.md) | Extension boundaries between core, plugins, and configuration | accepted |
| [0011](0011-jsonrpc-envelope-daemon.md) | JSON-RPC envelope, framing, transport, and daemon lifecycle | accepted |
| [0012](0012-resumable-transfers.md) | Resumable transfers, `.norte-partial`, and garbage collection | accepted |
| [0013](0013-sftp-provider.md) | SFTP provider, hostile-server containment, and testing | accepted |
| [0014](0014-ftp-provider.md) | FTP provider, MLSD, in-process testing, and cleartext risks | accepted |
| [0015](0015-connections-secrets.md) | Connections, secrets, TOFU, and Ed25519 | accepted |
| [0016](0016-object-storage-provider.md) | Object storage with OpenDAL and an S3-first key model | accepted |
| [0017](0017-fs-list-cursor-pagination.md) | Connection-scoped cursor pagination for `fs.list` | accepted |
| [0018](0018-archive-provider.md) | Read-only ZIP/TAR archives as virtual directories | accepted |
| [0019](0019-remote-logical-trash.md) | Logical `.norte-trash/` for remote providers | accepted |
| [0020](0020-norte-theme.md) | Shared semantic themes and terminal colour fallback | accepted |
| [0021](0021-cargo-dist-releases.md) | Prebuilt releases and cargo-dist installers | accepted |
| [0022](0022-wasm-plugin-host-manifest.md) | WASM plugin host, manifest, capabilities, and extension levels | accepted |
| [0023](0023-sqlite-journal-hash-chain.md) | SQLite WAL journal with an application-level hash chain | accepted |
| [0024](0024-norte-mcp-stdio-bridge.md) | SDK-free MCP stdio bridge to the daemon | accepted |
| [0025](0025-journal-hmac-anchors-audit-export.md) | HMAC journal-head anchoring and audit export | accepted |
| [0026](0026-lua-scripting-mlua.md) | Embedded Lua scripting with mlua | accepted |
| [0027](0027-gpui-feasibility.md) | GPUI feasibility decision for the GUI | accepted |
| [0028](0028-targz-compound-format.md) | `tar+gz` as an opaque compound archive format | accepted |
| [0029](0029-remote-session-lifecycle.md) | Remote session lifecycle: lazy eviction, single-flight, canonical dedup | accepted |
| [0030](0030-own-zip-cd-parser.md) | Own zip central-directory parser | accepted |
| [0031](0031-ai-subsystem.md) | AI subsystem: norte-ai providers, reviewable AI rename, semantic index | accepted |
| [0032](0032-plugin-provider-interface.md) | Plugin provider interface (WIT projection of the `Provider` trait) | proposed |
| [0033](0033-ftp-provider-as-plugin.md) | FTP provider as an embedded WASM plugin | accepted |
| [0034](0034-index-fts5.md) | Search index: `norte-index` crate with SQLite FTS5 | accepted |
| [0035](0035-norte-config-crate.md) | Shared norte-config crate and unified configuration resolution | accepted |
| [0036](0036-effects-schema-v1.md) | GUI effects schema v1 for theme `[effects]` | accepted |
| [0037](0037-plugin-data-out-v2.md) | Plugin data-out v2: styled previews, decorators, columns | accepted |
| [0038](0038-protocol-json-schema-and-semver-gate.md) | Protocol JSON Schema artifact and cargo-semver-checks gate | accepted |
| [0039](0039-provider-attributes-wire.md) | Provider attributes on the wire (typed, on-demand, degrading) | accepted |
| [0040](0040-help-corpus-and-markdown-lite.md) | Help corpus and markdown-lite | accepted |
| [0041](0041-core-providers-versus-plugin-providers.md) | Which providers live in the core, and which arrive as plugins | accepted |
| [0042](0042-batch-rename-wire.md) | Two-phase batch rename: a reviewed plan, executed by hash | accepted |
| [0043](0043-keymap-availability-and-the-mod-alias.md) | Keymap availability is declared, and `mod+` is a process policy | accepted |
| [0044](0044-numeric-counts-and-the-sacred-keys.md) | A count repeats the dispatch, and two keys are not for sale | accepted |
| [0045](0045-dialog-context-inheritance.md) | norte's dialogs stay norte's: `dialog_from`, one level, presets only | accepted |
| [0046](0046-journal-format-marker-in-chain.md) | The journal declares its format inside the hash chain | accepted |
| [0047](0047-volume-label-bytes-on-the-wire.md) | Volume label crosses the wire as bytes, not `String` | accepted |
| [0048](0048-comparison-confidence-on-the-wire.md) | A comparison declares what its criterion is worth | accepted |
| [0049](0049-the-retained-sync-plan.md) | The approved synchronisation plan is retained, and `sync.apply` carries nothing but its hash | accepted |
| [0050](0050-the-agent-plans-and-does-not-apply.md) | The agent plans and does not apply | accepted |
| [0051](0051-shared-fold-key-and-hash-framing.md) | The fold key moves to the permissive layer; the journal's framing stays and is pinned equal | accepted |
| [0052](0052-protected-roots-no-grant-reaches.md) | The daemon's own state directory is a root no grant reaches | accepted |
| [0053](0053-a-pairing-that-may-join-two-files-is-not-a-step.md) | A pairing that may join two files is skipped, not acted on | accepted |
| [0054](0054-a-provider-answers-about-a-location.md) | A provider answers about a location, not only about itself | accepted |
| [0055](0055-a-daemon-may-tell-a-client-to-start-its-replacement.md) | A daemon may tell a client to start its replacement | accepted |
| [0056](0056-a-provider-may-delegate-to-a-program-it-does-not-trust.md) | A provider may delegate to a program it does not trust | accepted |
| [0057](0057-a-plugin-may-be-given-a-location-it-cannot-name.md) | A plugin may be given a location it cannot name | accepted |
| [0058](0058-a-screen-is-a-tree-the-core-keeps-and-does-not-read.md) | A screen is a tree the core keeps and does not read | accepted |
| [0059](0059-the-session-is-a-document-with-one-writer.md) | The session is a document with one writer | accepted |
| [0060](0060-writing-an-archive-is-not-writing-into-one.md) | Writing an archive is not writing into one | accepted |
| [0061](0061-a-configuration-name-that-becomes-a-filename.md) | A configuration name that becomes a filename is bytes, and resolves byte-exactly | accepted |
| [0062](0062-the-session-schema-version-has-one-home.md) | The session's schema version has one home, and the core refuses what it cannot read | accepted |
| [0063](0063-a-result-that-travels-as-progress.md) | A result that travels as progress, and a connection closed by path | accepted |
| [0064](0064-what-a-plan-can-promise-about-the-destination.md) | What a plan can promise about the destination | accepted |
| [0065](0065-a-frontend-is-retired-before-its-replacement-exists.md) | A frontend is retired before its replacement exists | accepted |
| [0066](0066-renderers-use-a-rust-ui-host.md) | Renderers use a Rust UI host, and a client SDK below it | accepted |
| [0067](0067-the-renderer-is-a-painter-not-a-framework.md) | The reference renderer paints, and brings no framework to do it | accepted |
| [0068](0068-a-row-is-named-by-key-and-generation.md) | A row is named by key AND generation, and the bridge breaks on purpose | accepted |
| [0069](0069-how-image-bytes-reach-the-webview.md) | How image bytes reach the webview, and what the window still refuses to do | accepted |
| [0070](0070-a-mutation-names-its-own-operands.md) | A mutation names its own operands, and the surface that approves it labels out of band | accepted |
| [0071](0071-a-number-a-name-and-an-anchor-that-could-not-say-what-they-were.md) | A number, a name and an anchor that could not say what they were | accepted |
| [0072](0072-the-approved-directory-is-the-root-a-leaf-hangs-from.md) | The approved directory is the root a leaf hangs from | accepted |
| [0073](0073-the-directory-the-human-was-looking-at.md) | The directory the human was looking at | accepted |
| [0074](0074-a-drop-is-a-list-someone-else-wrote.md) | A drop is a list someone else wrote | accepted |
| [0075](0075-the-tree-does-not-move-when-the-listing-does.md) | The tree does not move when the listing does | accepted |
| [0076](0076-creating-a-file-is-a-mutation-like-any-other.md) | Creating a file is a mutation like any other | accepted |
| [0077](0077-the-same-command-means-the-same-thing-in-both-frontends.md) | The same command means the same thing in both frontends | accepted |
| [0078](0078-a-name-that-travels-intact-and-still-means-something-else.md) | A name that travels intact and still means something else | accepted |
| [0079](0079-a-profile-declares-it-does-not-execute.md) | A profile declares, it does not execute | accepted |
| [0080](0080-a-digest-is-a-read-that-nobody-can-take-back.md) | A digest is a read that nobody can take back | accepted |
| [0081](0081-permissions-are-a-mutation-and-carry-their-way-back.md) | Permissions are a mutation, and they carry their way back | accepted |
| [0082](0082-what-norte-hands-to-a-program-it-does-not-own.md) | What norte hands to a program it does not own | accepted |
| [0083](0083-permissions-down-a-tree-are-two-modes-and-one-batch.md) | Permissions down a tree are two modes and one batch | accepted |
| [0084](0084-the-shell-behind-the-panels-is-a-process-not-a-scrollback.md) | The shell behind the panels is a process, not a scrollback | accepted |
| [0085](0085-an-async-test-waits-for-an-event-not-for-the-clock.md) | An async test waits for an event, not for the clock | accepted |
| [0086](0086-the-single-writer-is-one-actor-not-one-file.md) | The single writer is one actor, not one file | accepted |
| [0087](0087-the-window-is-a-supported-frontend-and-has-a-gate-that-runs.md) | The window is a supported frontend, and has a gate that runs | accepted |
| [0088](0088-a-declared-capability-that-nobody-honours-is-a-lie.md) | A declared capability that nobody honours is a lie | accepted |
| [0089](0089-the-protocol-gets-a-catalogue-so-forgetting-a-surface-turns-red.md) | The protocol gets a catalogue, so forgetting a surface turns red | accepted |
| [0090](0090-why-a-connection-failed-is-a-notification-not-an-error-field.md) | Why a connection failed is a notification, not a field on the error | accepted |
| [0091](0091-a-password-does-not-cross-a-bridge-on-every-keystroke.md) | A password does not cross a bridge on every keystroke | accepted |
| [0092](0092-a-log-is-pulled-with-a-cursor-and-its-level-is-raised-by-its-owner.md) | A log is pulled with a cursor, and its level is raised by whoever owns the ring | accepted |
| [0093](0093-a-provider-plugin-serves-the-scheme-it-declares.md) | A provider plugin serves the scheme it declares | accepted |
| [0094](0094-a-plugin-says-which-wit-it-was-built-against.md) | A plugin says which WIT it was built against, and the host says whether it serves it | accepted |
| [0095](0095-a-renamer-plugin-proposes-and-the-core-renames.md) | A renamer plugin proposes, and the core renames | accepted |
| [0096](0096-what-is-operated-on-and-what-is-pointed-at-are-two-questions.md) | What is operated on and what is pointed at are two questions | accepted |
| [0097](0097-parity-between-the-terminal-and-the-window-is-a-test.md) | Parity between the terminal and the window is a test, not a convention | accepted |
| [0098](0098-profile-start-is-a-seed-and-the-session-wins.md) | `[profile.start]` is a seed in wire form, and the session wins | accepted |
| [0099](0099-the-window-reloads-what-a-profile-switch-reloads.md) | The window reloads what a profile switch reloads, and says the rest | accepted |
| [0100](0100-a-hook-observes-what-the-journal-recorded-and-may-only-speak.md) | A hook observes what the journal recorded, and may only speak | accepted |
| [0101](0101-a-hook-may-write-a-sidecar-through-the-policy-engine-as-a-plugin-actor.md) | A hook may write a sidecar, through the policy engine, as a plugin actor | accepted |
| [0102](0102-a-side-panel-follows-and-tab-is-the-listing-ring.md) | A side panel FOLLOWS the active listing, and `Tab` is the listing ring | accepted |
| [0103](0103-a-modal-line-declares-its-role-and-the-default-scheme-goes-unsaid.md) | A modal line declares its ROLE, and the default scheme goes unsaid | accepted |
