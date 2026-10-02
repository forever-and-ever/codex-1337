use std::fs::File;
use std::fs::TryLockError;
use std::path::Path;
use std::time::Duration;

pub fn account_nickname(input: &str) -> std::io::Result<&str> {
    let nickname = input.trim();
    if nickname.chars().count() > 64
        || nickname
            .chars()
            .any(|c| c.is_control() || matches!(c as u32, 0x202A..=0x202E | 0x2066..=0x2069))
    {
        return Err(std::io::Error::other(
            "Account nickname must be at most 64 characters and contain no control text",
        ));
    }
    Ok(nickname)
}

/// Serializes subscription-account defaults and nickname edits across processes.
/// Retain the returned file until the configuration transaction finishes.
pub async fn lock_account_preferences(codex_home: &Path) -> std::io::Result<File> {
    let path = codex_home.join("claude-accounts.lock");
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    tokio::time::timeout(Duration::from_secs(/*secs*/ 60), async {
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(()),
                Err(TryLockError::WouldBlock) => {
                    tokio::time::sleep(Duration::from_millis(/*millis*/ 25)).await;
                }
                Err(error) => return Err(std::io::Error::from(error)),
            }
        }
    })
    .await
    .map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "Account preferences lock timed out",
        )
    })??;
    Ok(file)
}
