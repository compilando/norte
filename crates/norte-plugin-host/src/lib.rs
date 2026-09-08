//! Host de plugins WASM de norte (ADR 0022): descubre plugins locales, valida
//! sus manifiestos y modela sus capabilities y su catálogo ordenado.
//!
//! Esta fase (M4-P1) trae el MODELO — manifiesto, capabilities, catálogo — sin
//! el runtime: `wasmtime` + Component Model + las interfaces WIT llegan en
//! M4-P2. El modelo es lo que consumen tanto el runtime como el gestor de
//! extensiones (la vista tipo `VSCode`, M4-P3).
//!
//! Invariante dura (spec §7.1): un plugin JAMÁS tiene `exec` — el manifiesto lo
//! rechaza al parsear.
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
mod wit_imports;

pub use capability::{Capabilities, LocationCap, NetCap, Scope};
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
    DecoratorContrib, HOOK_EVENTS, HookContrib, Manifest, ManifestError, PreviewerContrib,
    ProviderContrib, RenamerContrib, is_valid_plugin_id, scheme_claimable,
};
pub use runtime::{
    ColumnsInstance, DecoratorInstance, HookInstance, LocationHost, MAX_ARTIFACT_BYTES,
    MAX_HOOK_EFFECTS, MAX_RENAME_PROPOSALS, PluginInstance, PluginRuntime, ProviderInstance,
    RenamerInstance, RuntimeError, columns_iface, decorator_iface, hook_iface, location_iface,
    previewer_iface, provider_iface, renamer_iface,
};
/// Handle opaco de un `writer` resource del guest (#30 stage 2b-write): el
/// adapter host lo lleva en su `ByteSink` y lo pasa a los métodos
/// `writer_*`/`writer_drop` de [`ProviderInstance`].
pub use wasmtime::component::ResourceAny as WriterHandle;
pub use wit_imports::{SERVED_WIT, WitMismatch, wit_mismatch, wit_packages};
