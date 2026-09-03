# Plugin template

The smallest norte plugin that builds, installs and runs. It is a previewer
for `text/plain` and a `hello` command, with one setting. Its purpose is to
be copied.

## Start your plugin from it

1. Copy this directory anywhere. Inside the norte repository the `wit`
   symlink keeps pointing at the host's WIT; outside it, replace the symlink
   with a copy of `crates/norte-plugin-host/wit/`.
2. In `plugin.toml`, change `id` (reverse-DNS, yours), `name`, `publisher`
   and `description`. Keep `category` and the contributions until your code
   replaces them.
3. In `Cargo.toml`, change `name`.
4. Replace the bodies in `src/lib.rs`. The world you implement stays
   `norte-plugin` unless you write a decorator, columns or provider plugin —
   those have their own worlds (`norte-decorator`, `norte-columns`,
   `norte-provider`) and a different `category`.
5. Build, stage, install:

   ```sh
   rustup target add wasm32-wasip2
   cargo build --release --target wasm32-wasip2
   mkdir -p stage
   cp plugin.toml help.md stage/
   cp target/wasm32-wasip2/release/norte_plugin_template.wasm stage/plugin.wasm
   norte plugin install stage
   ```

6. Approve it and switch it on in the extension manager (`F12` in the TUI),
   then `norte plugin run org.example.template hello world`.

The whole story — manifest fields, capabilities, config, help pages,
diagnostics and what a WIT bump means for your binary — is in
`docs/plugins.md`.
