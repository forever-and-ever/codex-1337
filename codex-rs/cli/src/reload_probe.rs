//! Keep the legacy transport probe stable while exposing checkpoint-reader versions.

pub(crate) fn run(checkpoint_versions: bool) -> anyhow::Result<()> {
    if checkpoint_versions {
        println!(
            "{}",
            serde_json::to_string(codex_tui::UI_RELOAD_CHECKPOINT_VERSIONS)?
        );
    } else {
        println!("{}", codex_tui::UI_RELOAD_PROTOCOL);
    }
    Ok(())
}
