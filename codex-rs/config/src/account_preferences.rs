use std::fs::File;
use std::fs::TryLockError;
use std::path::Path;
use std::time::Duration;

/// Read the account preferences written by `/account`, rather than a long-running turn's
/// startup snapshot. These user preferences supply defaults only, never replace a thread binding.
pub async fn read_account_preferences(
    codex_home: &Path,
) -> std::io::Result<Option<crate::config_toml::WranglerConfig>> {
    let text = match tokio::fs::read_to_string(codex_home.join(crate::CONFIG_TOML_FILE)).await {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut config: toml::Table = toml::from_str(&text).map_err(|_| {
        std::io::Error::other("Cannot read account preferences: invalid config.toml")
    })?;
    config
        .remove("wrangler")
        .map(|value| {
            value
                .try_into()
                .map_err(|_| std::io::Error::other("Invalid [wrangler] account preferences"))
        })
        .transpose()
}

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
