use crate::cli::Color;
use crate::cli::CyberAccessProgramCliArg;
use crate::cli::ExecSharedCliOptions;
use crate::cli::ForkArgs;
use crate::cli::ResumeArgs;
use crate::cli::ReviewArgs;
use clap::Args;
use clap::Id;
use clap::Parser;
use clap::builder::Resettable;
use codex_protocol::protocol::ThreadSource;
use codex_utils_cli::CliConfigOverrides;
use codex_utils_cli::SharedCliOptions;
use std::path::PathBuf;

// TELOS(explicit-command-verbs): a missing or unknown verb cannot start a task.
#[expect(
    clippy::struct_excessive_bools,
    reason = "Independent command-line switches are orthogonal and clap populates their boolean parser fields"
)]
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

    /// Request an experimental Cyber access program for this turn (`OpenAI` provider only).
    /// Omit to use server defaults. Not supported with review; fork requires a prompt.
    #[arg(long, value_enum, value_name = "PROGRAM", global = true)]
    pub cyber_access_program: Option<CyberAccessProgramCliArg>,

    /// Allow running Codex outside a Git repository.
    #[arg(long = "skip-git-repo-check", global = true, default_value_t = false)]
    pub skip_git_repo_check: bool,

    /// Save trust for this project and future chats, including MCP commands outside the sandbox.
    /// Persists even with `--ephemeral`; an explicitly untrusted project still rejects it.
    #[arg(long, global = true)]
    pub trust: bool,

    /// Run without persisting session files to disk.
    #[arg(long = "ephemeral", global = true, default_value_t = false)]
    pub ephemeral: bool,

    /// Do not load `CODEX_HOME/config.toml`; auth still uses `CODEX_HOME`.
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

pub(crate) fn augment_exec_shared_args(cmd: clap::Command, for_update: bool) -> clap::Command {
    // Shared options inherit through the typed root/exec merge, not clap's global ID propagation.
    let schema = clap::Command::new("exec-shared");
    let schema = if for_update {
        SharedCliOptions::augment_args_for_update(schema)
    } else {
        SharedCliOptions::augment_args(schema)
    };
    let args = schema.get_arguments().map(|arg| {
        let conflicts = schema
            .get_arg_conflicts_with(arg)
            .into_iter()
            .map(|conflict| exec_shared_id(conflict.get_id().as_str()));
        arg.clone()
            .id(exec_shared_id(arg.get_id().as_str()))
            .conflicts_with(Resettable::<Id>::Reset)
            .conflicts_with_all(conflicts)
            // Images belong to each command; global copies duplicate resumed attachments.
            .global(arg.get_id().as_str() != "images")
    });
    let groups = schema.get_groups().map(|group| {
        let members = group.get_args().map(|id| exec_shared_id(id.as_str()));
        group.clone().arg(Resettable::<Id>::Reset).args(members)
    });
    cmd.args(args).groups(groups)
}

fn exec_shared_id(id: &str) -> &'static str {
    match id {
        "images" => "exec_images",
        "model" => "exec_model",
        "oss" => "exec_oss",
        "oss_provider" => "exec_oss_provider",
        "config_profile_v2" => "exec_config_profile_v2",
        "sandbox_mode" => "exec_sandbox_mode",
        "auto_review" => "exec_auto_review",
        "dangerously_bypass_approvals_and_sandbox" => {
            "exec_dangerously_bypass_approvals_and_sandbox"
        }
        "bypass_hook_trust" => "exec_bypass_hook_trust",
        "cwd" => "exec_cwd",
        "add_dir" => "exec_add_dir",
        _ => unreachable!(
            "shared CLI schema contains an argument without an exec parser scope: {id}"
        ),
    }
}

pub(crate) fn parse_exec_shared_args(
    matches: &clap::ArgMatches,
    current: &SharedCliOptions,
) -> SharedCliOptions {
    macro_rules! optional {
        ($field:ident) => {{
            let id = exec_shared_id(stringify!($field));
            if matches.contains_id(id) {
                matches.get_one(id).cloned()
            } else {
                current.$field.clone()
            }
        }};
    }
    macro_rules! flag {
        ($field:ident) => {{
            let id = exec_shared_id(stringify!($field));
            matches.get_one(id).copied().unwrap_or(current.$field)
        }};
    }
    macro_rules! many {
        ($field:ident) => {{
            let id = exec_shared_id(stringify!($field));
            if matches.contains_id(id) {
                matches
                    .get_many(id)
                    .map(|values| values.cloned().collect())
                    .unwrap_or_default()
            } else {
                current.$field.clone()
            }
        }};
    }

    // Exhaustive construction forces newly shared fields to choose their parser scope.
    SharedCliOptions {
        images: many!(images),
        model: optional!(model),
        oss: flag!(oss),
        oss_provider: optional!(oss_provider),
        config_profile_v2: optional!(config_profile_v2),
        sandbox_mode: optional!(sandbox_mode),
        auto_review: flag!(auto_review),
        dangerously_bypass_approvals_and_sandbox: flag!(dangerously_bypass_approvals_and_sandbox),
        bypass_hook_trust: flag!(bypass_hook_trust),
        cwd: optional!(cwd),
        add_dir: many!(add_dir),
    }
}
