//! Test binaries embed the app-server runtime that the `codex` binary installs.

#[ctor::ctor]
fn install_embedded_app_server() {
    codex_app_server::install_embedded_runtime();
}
