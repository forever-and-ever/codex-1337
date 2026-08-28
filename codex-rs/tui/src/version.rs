/// The current Codex CLI version as embedded at compile time.
pub const CODEX_CLI_VERSION: &str = env!("CARGO_PKG_VERSION");

/// The owned distribution identity shown on Codex's user-facing surfaces.
pub const CODEX_DISPLAY_VERSION: &str = concat!(env!("CARGO_PKG_VERSION"), "-wrangler");
