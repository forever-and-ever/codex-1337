use std::io;
use std::process::Stdio;
use std::time::Duration;

use tokio::process::Command;

#[derive(Debug)]
pub struct CommandBackend(pub Vec<String>);

impl CommandBackend {
    pub fn notify(&self, message: &str) -> io::Result<()> {
        let Some((program, args)) = self.0.split_first() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "notification_command must contain an executable",
            ));
        };
        let mut child = Command::new(program)
            .args(args)
            .arg(message)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()?;
        // The helper may await a desktop action; it must not block input or frame rendering.
        tokio::spawn(async move {
            match tokio::time::timeout(Duration::from_secs(/*secs*/ 360), child.wait()).await {
                Ok(Ok(status)) if status.success() => {}
                Ok(Ok(status)) => tracing::warn!(%status, "Notification command failed"),
                Ok(Err(error)) => tracing::warn!(%error, "Failed to reap notification command"),
                Err(_) => {
                    tracing::warn!("Notification command exceeded its six-minute lifetime");
                    if let Err(error) = child.kill().await {
                        tracing::warn!(%error, "Failed to kill notification command");
                    }
                }
            }
        });
        Ok(())
    }
}

#[cfg(all(test, unix))]
#[path = "command_tests.rs"]
mod tests;
