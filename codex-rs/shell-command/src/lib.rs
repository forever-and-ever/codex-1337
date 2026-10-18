//! Command parsing and safety utilities shared across Codex crates.

pub mod agent_shell;
pub mod shell_detect;
pub mod shell_snapshot;
mod startup;

pub use startup::shell_startup_script;

pub mod bash;
pub(crate) mod command_safety;
pub mod parse_command;
pub mod powershell;
pub mod read_intent;

pub use command_safety::is_dangerous_command;
