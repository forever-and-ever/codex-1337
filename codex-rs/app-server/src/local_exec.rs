use clap::Args;
use codex_sandboxing::LinuxSandboxPidNamespace;

// TELOS(managed-process-visibility): Only trusted daemon startup selects host PID visibility for local tools.

/// Trusted startup settings for this app server's local executor, not remote executors.
#[derive(Args, Debug, Clone, Default, PartialEq, Eq)]
pub struct AppServerLocalExecArgs {
    /// Linux PID namespace for local tools: isolate (default) or inherit.
    /// Inherit exposes host processes and allows signals to other same-UID processes.
    #[arg(long, value_name = "MODE", default_value = "isolate")]
    pub linux_sandbox_pid_namespace: LinuxSandboxPidNamespace,
}
