fn main() -> anyhow::Result<()> {
    codex_app_server::install_embedded_runtime();
    codex_cli::main()
}
