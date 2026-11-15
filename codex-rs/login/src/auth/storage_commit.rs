//! One stable transaction lock for account edits, refresh CAS and logout.
use super::*;
use std::fs::OpenOptions;
use std::fs::TryLockError;
use std::time::Duration;
use std::time::Instant;

pub(super) fn lock(home: &Path) -> std::io::Result<File> {
    let auth_file = get_auth_file(home);
    // Keyring commits can unlink an intermediate symlink used by another
    // home, so resolvable chains must share the target's commit lock.
    let path = match storage_file::resolve_auth_file(auth_file.clone()) {
        Ok(path) => path,
        // Unresolvable fallback links can still be removed by keyring commits.
        Err(_) => auth_file,
    }
    .with_file_name("auth.lock");
    std::fs::create_dir_all(
        path.parent()
            .ok_or_else(|| std::io::Error::other("credential lock has no parent"))?,
    )?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match file.try_lock() {
            Ok(()) => return Ok(file),
            Err(TryLockError::WouldBlock) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10))
            }
            Err(TryLockError::WouldBlock) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "credential commit lock timed out",
                ));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

#[cfg(test)]
#[path = "storage_commit_tests.rs"]
mod tests;
