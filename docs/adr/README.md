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
| [0104](0104-an-extension-is-uninstalled-from-the-manager-and-the-manager-has-buttons.md) | An extension is uninstalled from the manager, and the manager has buttons | accepted, gaps closed by 0113 |
| [0105](0105-an-icon-is-a-column-left-of-the-name-and-a-decorator-says-what-it-is.md) | An icon is a column left of the name, and a decorator is told what an entry is | accepted |
| [0106](0106-the-chrome-is-derived-from-the-keymap-and-the-catalogue-not-drawn.md) | The chrome is derived from the keymap and the catalogue, not drawn | accepted |
| [0107](0107-a-thumbnail-is-a-plugin-kind-of-its-own-package.md) | A thumbnail is a plugin kind, in a WIT package of its own | accepted |
| [0108](0108-a-plugin-names-a-meaning-and-the-window-derives-its-chrome.md) | A plugin names a meaning, and the window derives its chrome | accepted |
| [0109](0109-a-theme-name-can-be-a-file-and-a-vscode-theme-imports-over-a-base.md) | A theme name can be a file you own, and a VSCode theme imports over a base | accepted |
| [0110](0110-lua-stays-in-the-terminal-and-the-window-says-so.md) | Lua stays in the terminal, and the window says so | accepted |
| [0111](0111-the-window-package-is-built-on-ubuntu-22-04.md) | The window package is built on Ubuntu 22.04, and that sets its floor | superseded in part by 0112 |
| [0112](0112-release-artefacts-are-built-in-one-pinned-image-and-smoked-per-distribution.md) | Release artefacts are built in one pinned image and smoked per distribution | accepted |
| [0113](0113-uninstall-reaches-the-daemon-and-a-broken-extension-is-a-row.md) | Uninstall reaches the daemon, and a broken extension is a row | accepted |
| [0114](0114-a-panel-history-is-walked-listed-marked-and-counted.md) | A panel's history is walked, listed, marked and counted | accepted |
| [0115](0115-the-start-screen-is-the-hosts-and-a-panel-that-opens-itself-closes-itself.md) | The start screen is the host's, and a panel that opens itself closes itself | accepted |
| [0116](0116-a-plugin-describes-a-panel-and-norte-paints-it.md) | A plugin describes a panel, and norte paints it | accepted |
| [0117](0117-the-disk-map-is-a-task-with-a-report-and-the-core-measures-it.md) | The disk map is a task with a report, and the core measures it | accepted |
| [0118](0118-imagenes-en-la-tui.md) | The TUI paints an image as pixels, outside ratatui, or falls back | accepted |
| [0124](0124-columns-give-way-so-the-name-can-be-read.md) | Columns give way so the name can be read | accepted |
| [0125](0125-menus-are-read-in-sections.md) | Menus are read in sections | accepted |
| [0126](0126-the-catalogue-declares-what-a-command-does.md) | The catalogue declares what a command does | accepted |
| [0127](0127-logs-are-structured-and-a-task-is-logged-inside-its-request.md) | Logs are structured, and a task is logged inside the request that asked for it | accepted |
| [0128](0128-the-listing-reads-in-bands-and-says-what-it-may-do.md) | The listing reads in bands, and says what it may do | accepted |
| [0129](0129-settings-have-sections-and-resetting-tells-the-truth.md) | Settings have sections, and resetting tells the truth | accepted |
| [0130](0130-the-window-edits-settings-with-its-own-controls.md) | The window edits settings with its own controls | accepted |
| [0131](0131-the-window-has-an-activity-bar-and-no-key-bar.md) | The window has an activity bar and no key bar | accepted |
| [0132](0132-the-status-bar-is-made-of-items.md) | The status bar is made of items | accepted |
| [0133](0133-layout-and-tab-buttons-run-existing-commands.md) | Layout and tab buttons run existing commands | accepted |
| [0134](0134-panels-on-one-edge-share-it-as-tabs.md) | Panels on one edge share it as tabs | accepted |
| [0135](0135-the-scrollbar-carries-a-marks-ruler.md) | The scrollbar carries a marks ruler | accepted |
| [0136](0136-an-optional-custom-title-bar.md) | An optional custom title bar | accepted |
| [0137](0137-plugins-contribute-status-items-through-their-columns.md) | Plugins contribute status items through their columns | accepted |
| [0138](0138-panels-move-by-dragging-and-splits-flip.md) | Panels move by dragging, and splits flip | accepted |
| [0139](0139-each-frontend-remembers-its-own-layout.md) | Each frontend remembers its own layout | accepted |
| [0140](0140-the-terminal-panel-column-draws-icons.md) | The terminal's panel column draws icons | accepted |
| [0141](0141-plugins-compile-once-and-the-viewer-opens-first.md) | Plugins compile once, and the viewer opens first | accepted |
| [0142](0142-plugin-binaries-are-checked-when-loaded.md) | Plugin binaries are checked when they are loaded | accepted |
| [0143](0143-a-dialog-can-carry-a-form.md) | A dialog can carry a form | accepted |
| [0144](0144-attribute-columns-sort-by-value.md) | Attribute columns sort by their value | accepted |
| [0145](0145-owner-and-group-by-name.md) | Owner and group by name | accepted |
| [0146](0146-a-light-progress-bar-in-the-status-bar.md) | A light progress bar in the status bar | accepted |
| [0147](0147-a-task-can-be-paused.md) | A task can be paused | accepted |
| [0148](0148-repeating-a-failed-transfer-and-the-thin-line.md) | Repeating a failed transfer, and the thin line | accepted |
| [0149](0149-a-serial-queue-for-transfers.md) | A serial queue for transfers | accepted |
| [0150](0150-rsa-client-keys-as-a-per-connection-opt-in.md) | RSA client keys as a per-connection opt-in | accepted |
| [0151](0151-a-destination-that-goes-away-mid-copy.md) | A destination that goes away mid-copy | accepted |
| [0152](0152-an-undo-of-a-creation-checks-what-it-is-about-to-delete.md) | An undo of a creation checks what it is about to delete | accepted |
| [0153](0153-the-subshell-is-moved-through-a-mailbox-not-by-typing.md) | The subshell is moved through a mailbox, not by typing | accepted |
| [0154](0154-the-source-code-is-written-in-english.md) | The source code is written in English | accepted |
| [0155](0155-typing-a-name-jumps-to-it-in-the-presets-that-reserve-letters.md) | Typing a name jumps to it, in the presets that reserve letters | accepted |
| [0156](0156-a-sync-plan-does-not-measure-the-orphans-it-copies.md) | A sync plan does not measure the orphans it copies | accepted |
| [0157](0157-native-builders-share-one-release-contract.md) | Native builders share one release contract, and VM state stays outside the tree | accepted |
| [0158](0158-a-confined-root-on-windows-opens-one-name-at-a-time.md) | A confined root on Windows opens one name at a time, and identity is a `NodeId` | accepted |
| [0159](0159-the-windows-daemon-listens-on-an-owner-only-named-pipe.md) | The Windows daemon listens on an owner-only named pipe, and its unsafe lives in `norte-winpipe` | accepted |
