use crate::shell_detect::DetectedShell;

/// TELOS(agent-bash): agent syntax is independent of the human's login shell. An absent Bash is
/// an execution failure, never permission to reinterpret a command as zsh or sh.
pub fn default_shell() -> DetectedShell {
    #[cfg(target_os = "linux")]
    {
        DetectedShell {
            shell_type: crate::shell_detect::ShellType::Bash,
            shell_path: which::which("bash")
                .unwrap_or_else(|_| std::path::PathBuf::from("/bin/bash")),
        }
    }
    #[cfg(not(target_os = "linux"))]
    crate::shell_detect::default_user_shell()
}
