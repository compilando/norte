//! norte's WASM plugin host (ADR 0022): discovers local plugins, validates
//! their manifests and models their capabilities and their ordered catalog.
//!
//! This phase (M4-P1) brings the MODEL — manifest, capabilities, catalog —
//! without the runtime: `wasmtime` + Component Model + the WIT interfaces
//! arrive in M4-P2. The model is what both the runtime and the extensions
//! manager (the `VSCode`-like view, M4-P3) consume.
//!
//! Hard invariant (spec §7.1): a plugin NEVER has `exec` — the manifest
//! rejects it while parsing.
//!
//! ```
//! use norte_plugin_host::{Manifest, Category};
//! let m = Manifest::from_toml(r#"
//!     [plugin]
//!     id = "org.norte.demo"
//!     name = "Demo"
//!     publisher = "norte"
//!     version = "0.1.0"
//!     category = "command"
//!     [capabilities]
//!     fs-read = "scoped"
//! "#).unwrap();
//! assert_eq!(m.category, Category::Command);
//! assert_eq!(m.capabilities.badges(), vec!["fs-read".to_owned()]);
//! ```
#![forbid(unsafe_code)]
#![warn(missing_docs)]

mod bindings;
mod capability;
mod catalog;
mod config_values;
mod manifest;
mod runtime;
mod thumb;
mod wit_imports;

pub use capability::{Capabilities, FsWriteCap, LocationCap, NetCap, Scope, SidecarList};
pub use catalog::{
    Catalog, HelpPresence, LoadError, PluginEntry, Tier, verified_child, wasm_digest_of,
};
pub use config_values::{
    CONFIG_VALUES_MAX_BYTES, ConfigValueError, encode_wire_value, persist_plugin_setting,
    persist_plugin_setting_typed, resolve_settings,
};
pub use manifest::{
    COMMAND_ID_MAX_CHARS, COMMAND_MAX_COUNT, COMMAND_TITLE_MAX_CHARS, CONFIG_DESCRIPTION_MAX_CHARS,
    CONFIG_ENUM_MAX_VALUES, CONFIG_KEY_MAX_CHARS, CONFIG_MAX_KEYS, CONFIG_STRING_MAX_CHARS,
    CORE_SCHEMES, Category, ColumnContrib, CommandContrib, ConfigKeySpec, Contributions,
    DecoratorContrib, DecoratorSlot, HOOK_EVENTS, HookContrib, Manifest, ManifestError,
    PanelContrib, PreviewerContrib, ProviderContrib, RenamerContrib, SIDECAR_MAX_NAMES,
    ThumbnailContrib, is_valid_plugin_id, is_valid_sidecar_name, scheme_claimable,
};
pub use runtime::{
    ColumnsInstance, DecoratorInstance, HookInstance, LocationHost, MAX_ARTIFACT_BYTES,
    MAX_HOOK_EFFECTS, MAX_RENAME_PROPOSALS, MAX_SIDECAR_BYTES, MAX_SIDECAR_EFFECTS,
    OrganizerInstance, PanelFrame, PanelInstance, PluginInstance, PluginRuntime, ProviderInstance,
    RenamerInstance, RuntimeError, THUMB_MAX_BYTES, THUMB_MAX_EDGE, Thumbnail, ThumbnailInstance,
    WasmArtifact, columns_iface, decorator_iface, hook_iface, location_iface, organizer_iface,
    panel_iface, previewer_iface, provider_iface, renamer_iface, thumbnail_iface,
};
/// Opaque handle to a guest `writer` resource (#30 stage 2b-write): the host
/// adapter carries it in its `ByteSink` and passes it to the
/// `writer_*`/`writer_drop` methods of [`ProviderInstance`].
pub use wasmtime::component::ResourceAny as WriterHandle;
pub use wit_imports::{SERVED_WIT, WitMismatch, wit_mismatch, wit_packages};
