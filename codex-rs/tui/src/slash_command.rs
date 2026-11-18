use strum::IntoEnumIterator;
use strum_macros::AsRefStr;
use strum_macros::EnumIter;
use strum_macros::EnumString;
use strum_macros::IntoStaticStr;

/// Commands that can be invoked by starting a message with a leading slash.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, EnumString, EnumIter, AsRefStr, IntoStaticStr,
)]
#[strum(serialize_all = "kebab-case")]
pub enum SlashCommand {
    // DO NOT ALPHA-SORT! Enum order is presentation order in the popup, so
    // more frequently used commands should be listed first.
    Model,
    Effort,
    Account,
    Presets,
    Daybreak,
    Permissions,
    Yolo,
    Keymap,
    Vim,
    #[strum(serialize = "setup-default-sandbox")]
    ElevateSandbox,
    Experimental,
    #[strum(to_string = "approve")]
    AutoReview,
    Memories,
    Skills,
    Hooks,
    Review,
    Rename,
    New,
    Archive,
    Delete,
    Resume,
    Fork,
    Farm,
    Jira,
    #[strum(serialize = "farm_fresh")]
    FarmFresh,
    App,
    Init,
    Compact,
    Config,
    Recap,
    Warm,
    Alarms,
    Plan,
    Voice,
    Goal,
    Agents,
    Side,
    Btw,
    Copy,
    Export,
    Raw,
    Diff,
    Mention,
    Status,
    Context,
    Comms,
    #[strum(serialize = "contextrole")]
    ContextRole,
    Daemon,
    Reload,
    Warnings,
    Cd,
    #[strum(to_string = "pwd", serialize = "cwd")]
    Pwd,
    Usage,
    DebugConfig,
    Title,
    Statusline,
    Theme,
    Shimmer,
    Mcp,
    Apps,
    Plugins,
    Logout,
    Quit,
    Exit,
    Rollout,
    Ps,
    #[strum(to_string = "stop", serialize = "clean")]
    Stop,
    Clear,
    TestApproval,
    #[strum(serialize = "subagents")]
    MultiAgents,
    // Debugging commands.
    #[strum(serialize = "debug-m-drop")]
    MemoryDrop,
    #[strum(serialize = "debug-m-update")]
    MemoryUpdate,
}

impl SlashCommand {
    /// User-visible description shown in the popup.
    pub fn description(self) -> &'static str {
        match self {
            SlashCommand::Config => "configure Codex preferences",
            SlashCommand::New => "start a new chat during a conversation",
            SlashCommand::Init => "create an AGENTS.md file with instructions for Codex",
            SlashCommand::Compact => "summarize conversation to prevent hitting the context limit",
            SlashCommand::Recap => "summarize the current conversation now",
            SlashCommand::Alarms => "view or disarm this session's alarms",
            SlashCommand::Warm => "keep this Claude conversation's cache warm, or stop",
            SlashCommand::Review => "review my current changes and find issues",
            SlashCommand::Rename => "rename the current thread",
            SlashCommand::Resume => "resume a saved chat",
            SlashCommand::Archive => "archive this session",
            SlashCommand::Delete => "permanently delete this session",
            SlashCommand::Clear => "clear the terminal and start a new chat",
            SlashCommand::Fork => "fork the current chat",
            SlashCommand::Farm => "delegate a task with the current conversation",
            SlashCommand::Jira => "browse this project's issues",
            SlashCommand::FarmFresh => "delegate a task without conversation history",
            SlashCommand::App => "continue this session in the Desktop app",
            SlashCommand::Quit | SlashCommand::Exit => "exit Codex",
            SlashCommand::Copy => "copy the last response or part of it",
            SlashCommand::Export => "export the conversation as markdown",
            SlashCommand::Raw => "toggle raw scrollback mode for copy-friendly terminal selection",
            SlashCommand::Diff => "show git diff (including untracked files)",
            SlashCommand::Mention => "mention a file",
            SlashCommand::Skills => "use skills to improve how Codex performs specific tasks",
            SlashCommand::Hooks => "view and manage lifecycle hooks",
            SlashCommand::Daemon => "Manage the local background server.",
            SlashCommand::Reload => "reload the UI without interrupting your agents",
            SlashCommand::Warnings => "view retained warnings and diagnostic details",
            SlashCommand::Comms => "show or change cross-context message access",
            SlashCommand::ContextRole => crate::chatwidget::context_role::CONTEXT_ROLE_DESCRIPTION,
            SlashCommand::Context => {
                "inspect the instructions, tools and injections retained in context"
            }
            SlashCommand::Status => "show current session configuration and token usage",
            SlashCommand::Cd => "change the current working directory",
            SlashCommand::Pwd => "show the current working directory",
            SlashCommand::Usage => "view account usage or use a usage limit reset",
            SlashCommand::DebugConfig => "show config layers and requirement sources for debugging",
            SlashCommand::Title => "configure which items appear in the terminal title",
            SlashCommand::Statusline => "configure which items appear in the status line",
            SlashCommand::Theme => "choose a syntax highlighting theme",
            SlashCommand::Shimmer => "choose the animation on the working rows",
            SlashCommand::Ps => "show this agent tree’s running processes",
            SlashCommand::Stop => "stop all background terminals",
            SlashCommand::MemoryDrop => "DO NOT USE",
            SlashCommand::MemoryUpdate => "DO NOT USE",
            SlashCommand::Model => "choose what model and reasoning effort to use",
            SlashCommand::Presets => "run or save reusable prompts and slash commands",
            SlashCommand::Effort => "choose this model's reasoning effort",
            SlashCommand::Account => {
                "choose this conversation's account; /account <alias> also works"
            }
            SlashCommand::Daybreak => "turn Daybreak on or off",
            SlashCommand::Plan => "switch to Plan mode",
            SlashCommand::Voice => "start or stop voice; use /voice settings to choose a voice",
            SlashCommand::Goal => crate::chatwidget::goal_command::GOAL_DESCRIPTION,
            SlashCommand::Agents => "open the agent command center",
            SlashCommand::MultiAgents => "switch between this session's subagents",
            SlashCommand::Side | SlashCommand::Btw => "start a side conversation",
            SlashCommand::Permissions => "choose what Codex is allowed to do",
            SlashCommand::Yolo => "enable Full Access without approvals for this session",
            SlashCommand::Keymap => "remap TUI shortcuts",
            SlashCommand::Vim => "toggle Vim mode for the composer",
            SlashCommand::ElevateSandbox => "set up elevated agent sandbox",
            SlashCommand::Experimental => "toggle experimental features",
            SlashCommand::AutoReview => "approve one retry of a recent auto-review denial",
            SlashCommand::Memories => "configure memory use and generation",
            SlashCommand::Mcp => "list MCP tools; use /mcp verbose or /mcp login <name>",
            SlashCommand::Apps => "manage apps",
            SlashCommand::Plugins => "browse plugins",
            SlashCommand::Logout => "log out of Codex",
            SlashCommand::Rollout => "print the rollout file path",
            SlashCommand::TestApproval => "test approval request",
        }
    }

    /// Command string without the leading '/'. Provided for compatibility with
    /// existing code that expects a method named `command()`.
    pub fn command(self) -> &'static str {
        self.into()
    }

    /// Whether this command supports inline args (for example `/review ...`).
    pub fn supports_inline_args(self) -> bool {
        matches!(
            self,
            SlashCommand::Review
                | SlashCommand::Comms
                | SlashCommand::ContextRole
                | SlashCommand::Account
                | SlashCommand::Model
                | SlashCommand::Permissions
                | SlashCommand::Yolo
                | SlashCommand::Title
                | SlashCommand::Statusline
                | SlashCommand::Rename
                | SlashCommand::New
                | SlashCommand::Clear
                | SlashCommand::Fork
                | SlashCommand::Farm
                | SlashCommand::FarmFresh
                | SlashCommand::Plan
                | SlashCommand::Goal
                | SlashCommand::Voice
                | SlashCommand::Keymap
                | SlashCommand::Mcp
                | SlashCommand::Export
                | SlashCommand::Raw
                | SlashCommand::Cd
                | SlashCommand::Pwd
                | SlashCommand::Usage
                | SlashCommand::Side
                | SlashCommand::Btw
                | SlashCommand::Resume
        )
    }

    /// Whether this command remains available inside an active side conversation.
    pub fn available_in_side_conversation(self) -> bool {
        matches!(
            self,
            SlashCommand::Copy
                | SlashCommand::Agents
                | SlashCommand::Export
                | SlashCommand::Raw
                | SlashCommand::Diff
                | SlashCommand::Mention
                | SlashCommand::Status
                | SlashCommand::Context
                | SlashCommand::Daemon
                | SlashCommand::Reload
                | SlashCommand::Warnings
                | SlashCommand::Pwd
                | SlashCommand::Usage
                | SlashCommand::Rename
        )
    }

    /// Whether dispatch needs thread state to validate this command before consuming its draft.
    /// The composer must defer busy-state rejection and draft clearing for these commands.
    pub(crate) fn requires_dispatch_validation(self) -> bool {
        matches!(
            self,
            SlashCommand::Yolo
                | SlashCommand::Fork
                | SlashCommand::Review
                | SlashCommand::Farm
                | SlashCommand::FarmFresh
        )
    }

    /// Commands that do not require a writable current thread. The server must still be connected.
    pub(crate) fn available_when_thread_unavailable(self) -> bool {
        matches!(
            self,
            SlashCommand::Account
                | SlashCommand::New
                | SlashCommand::Clear
                | SlashCommand::Resume
                | SlashCommand::Agents
                | SlashCommand::MultiAgents
                | SlashCommand::Quit
                | SlashCommand::Exit
                | SlashCommand::Status
                | SlashCommand::Warnings
                | SlashCommand::DebugConfig
                | SlashCommand::Pwd
                | SlashCommand::Rollout
                | SlashCommand::Copy
                | SlashCommand::Raw
        )
    }

    /// Whether this command can be run while a task is in progress.
    pub fn available_during_task(self) -> bool {
        match self {
            SlashCommand::New
            | SlashCommand::Delete
            | SlashCommand::Init
            | SlashCommand::Compact
            | SlashCommand::Recap
            | SlashCommand::Export
            | SlashCommand::Keymap
            | SlashCommand::Vim
            | SlashCommand::ElevateSandbox
            | SlashCommand::Experimental
            | SlashCommand::Memories
            | SlashCommand::Review
            | SlashCommand::Plan
            | SlashCommand::Cd
            | SlashCommand::Clear
            | SlashCommand::Logout
            | SlashCommand::MemoryDrop
            | SlashCommand::MemoryUpdate => false,
            SlashCommand::Diff
            | SlashCommand::Fork
            | SlashCommand::Account
            | SlashCommand::Archive
            | SlashCommand::Resume
            | SlashCommand::Model
            | SlashCommand::Effort
            | SlashCommand::Presets
            | SlashCommand::Daybreak
            | SlashCommand::Permissions
            | SlashCommand::Yolo
            | SlashCommand::Copy
            | SlashCommand::Raw
            | SlashCommand::Rename
            | SlashCommand::Mention
            | SlashCommand::Skills
            | SlashCommand::Hooks
            | SlashCommand::Status
            | SlashCommand::Context
            | SlashCommand::Comms
            | SlashCommand::ContextRole
            | SlashCommand::Warm
            | SlashCommand::Alarms
            | SlashCommand::Daemon
            | SlashCommand::Reload
            | SlashCommand::Warnings
            | SlashCommand::Pwd
            | SlashCommand::Usage
            | SlashCommand::DebugConfig
            | SlashCommand::Ps
            | SlashCommand::Stop
            | SlashCommand::App
            | SlashCommand::Goal
            | SlashCommand::Voice
            | SlashCommand::Mcp
            | SlashCommand::Apps
            | SlashCommand::Plugins
            | SlashCommand::Title
            | SlashCommand::Statusline
            | SlashCommand::AutoReview
            | SlashCommand::Quit
            | SlashCommand::Exit
            | SlashCommand::Side
            | SlashCommand::Btw => true,
            SlashCommand::Farm | SlashCommand::FarmFresh | SlashCommand::Jira => true,
            SlashCommand::Rollout => true,
            SlashCommand::TestApproval => true,
            SlashCommand::Agents | SlashCommand::MultiAgents => true,
            SlashCommand::Shimmer | SlashCommand::Theme => true,
            SlashCommand::Config => false,
        }
    }

    fn is_visible(self) -> bool {
        match self {
            SlashCommand::Copy => !cfg!(target_os = "android"),
            SlashCommand::App => cfg!(any(target_os = "macos", target_os = "windows")),
            SlashCommand::Voice => true,
            SlashCommand::Rollout | SlashCommand::TestApproval => cfg!(debug_assertions),
            _ => true,
        }
    }
}

/// Return all built-in commands in a Vec paired with their command string.
pub fn built_in_slash_commands() -> Vec<(&'static str, SlashCommand)> {
    SlashCommand::iter()
        .filter(|command| command.is_visible())
        .map(|c| (c.command(), c))
        .collect()
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;
    use std::str::FromStr;

    use super::SlashCommand;

    #[test]
    fn stop_command_is_canonical_name() {
        assert_eq!(SlashCommand::Stop.command(), "stop");
    }

    #[test]
    fn clean_alias_parses_to_stop_command() {
        assert_eq!(SlashCommand::from_str("clean"), Ok(SlashCommand::Stop));
    }

    #[test]
    fn certain_commands_are_available_during_task() {
        assert!(SlashCommand::Goal.available_during_task());
        assert!(SlashCommand::Title.available_during_task());
        assert!(SlashCommand::Statusline.available_during_task());
        assert!(SlashCommand::Raw.available_during_task());
        assert!(SlashCommand::Raw.available_in_side_conversation());
        assert!(SlashCommand::Raw.supports_inline_args());
        assert!(SlashCommand::App.available_during_task());
    }

    #[test]
    fn auto_review_command_is_approve() {
        assert_eq!(SlashCommand::AutoReview.command(), "approve");
        assert_eq!(
            SlashCommand::from_str("approve"),
            Ok(SlashCommand::AutoReview)
        );
    }
}
