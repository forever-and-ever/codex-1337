use crate::cli::Color;
use crate::cli::CyberAccessProgramCliArg;
use crate::cli::ExecSharedCliOptions;
use crate::cli::ForkArgs;
use crate::cli::ResumeArgs;
use crate::cli::ReviewArgs;
use clap::Args;
use clap::Parser;
use codex_protocol::protocol::ThreadSource;
use codex_utils_cli::CliConfigOverrides;
use std::path::PathBuf;

// TELOS(explicit-command-verbs): a missing or unknown verb cannot start a task.
#[derive(Parser, Debug)]
#[command(version)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,

    /// Error out when config.toml contains unrecognized fields.
    #[arg(long = "strict-config", global = true, default_value_t = false)]
    pub strict_config: bool,

    #[clap(flatten)]
    pub shared: ExecSharedCliOptions,

    /// Source classification for newly created or forked threads.
    #[arg(long = "thread-source", value_name = "SOURCE", global = true)]
    pub thread_source: Option<ThreadSource>,

    /// Request an experimental Cyber access program for this turn (OpenAI provider only).
    /// Omit to use server defaults. Not supported with review; fork requires a prompt.
    #[arg(long, value_enum, value_name = "PROGRAM", global = true)]
    pub cyber_access_program: Option<CyberAccessProgramCliArg>,

    /// Allow running Codex outside a Git repository.
    #[arg(long = "skip-git-repo-check", global = true, default_value_t = false)]
    pub skip_git_repo_check: bool,

    /// Run without persisting session files to disk.
    #[arg(long = "ephemeral", global = true, default_value_t = false)]
    pub ephemeral: bool,

    /// Do not load CODEX_HOME/config.toml; auth still uses CODEX_HOME.
    #[arg(long = "ignore-user-config", global = true, default_value_t = false)]
    pub ignore_user_config: bool,

    /// Do not load user or project execpolicy .rules files.
    #[arg(long = "ignore-rules", global = true, default_value_t = false)]
    pub ignore_rules: bool,

    /// Path to a JSON Schema describing the model's final response.
    #[arg(long = "output-schema", value_name = "FILE", global = true)]
    pub output_schema: Option<PathBuf>,

    #[clap(skip)]
    pub config_overrides: CliConfigOverrides,

    /// Specifies color settings for the output.
    #[arg(long = "color", value_enum, default_value_t = Color::Auto, global = true)]
    pub color: Color,

    /// Print events to stdout as JSONL.
    #[arg(
        long = "json",
        alias = "experimental-json",
        default_value_t = false,
        global = true
    )]
    pub json: bool,

    /// Write the last agent message to this file.
    #[arg(
        long = "output-last-message",
        short = 'o',
        value_name = "FILE",
        global = true
    )]
    pub last_message_file: Option<PathBuf>,
}

#[derive(Debug, clap::Subcommand)]
pub enum Command {
    /// Start a new non-interactive task, reading stdin when PROMPT is omitted or -.
    Run(RunArgs),
    /// Resume a previous session by id or pick the most recent with --last.
    Resume(ResumeArgs),
    /// Fork a previous session by id into a new session.
    Fork(ForkArgs),
    /// Run a code review against the current repository.
    Review(ReviewArgs),
}

#[derive(Args, Debug)]
pub struct RunArgs {
    /// Initial instructions; omitted or - reads stdin. Piped stdin is appended
    /// to an explicit prompt as a <stdin> block.
    #[arg(value_name = "PROMPT", value_hint = clap::ValueHint::Other)]
    pub prompt: Option<String>,

    /// Images attached to the new task.
    #[arg(
        long = "image",
        short = 'i',
        value_name = "FILE",
        value_delimiter = ','
    )]
    pub images: Vec<PathBuf>,
}

pub(crate) fn mark_exec_global_args(cmd: clap::Command) -> clap::Command {
    // Images belong to each command; global copies would duplicate resumed attachments.
    let ids = cmd
        .get_arguments()
        .filter(|arg| arg.get_id().as_str() != "images")
        .map(|arg| arg.get_id().clone())
        .collect::<Vec<_>>();
    ids.into_iter()
        .fold(cmd, |cmd, id| cmd.mut_arg(id, |arg| arg.global(true)))
}
