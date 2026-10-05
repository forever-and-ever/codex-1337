//! The host-local rendezvous root for privileged app-server RPC sockets.
//!
//! Every listener uses this root, which sandboxes hide even before a daemon
//! starts. It must not depend on HOME, TMPDIR, CODEX_HOME, or command settings.
//! Linux requires the login user's private /run/user/<uid> directory. Live IPC
//! lasts for the user session, not for the retention policy of scratch storage.

use std::fs;
use std::io;
use std::os::unix::fs::DirBuilderExt;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::path::PathBuf;

/// Returns the fixed executor-local directory that every sandbox must hide.
pub fn shared_daemon_socket_directory() -> io::Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Ok(user_runtime_directory().join("codex-1337/ipc"))
    }
    #[cfg(not(target_os = "linux"))]
    legacy_socket_directory()
}

#[cfg(target_os = "linux")]
fn user_runtime_directory() -> PathBuf {
    let uid = unsafe { libc::geteuid() };
    PathBuf::from(format!("/run/user/{uid}"))
}

fn legacy_socket_directory() -> io::Result<PathBuf> {
    let uid = unsafe { libc::geteuid() };
    // Resolve the system alias /tmp -> /private/tmp on macOS.
    Ok(fs::canonicalize("/tmp")?.join(format!("codex-daemon-{uid}")))
}

/// Creates the reserved directory, rejecting symlinks and unsafe existing owners or modes.
pub fn prepare_shared_daemon_socket_directory() -> io::Result<PathBuf> {
    let directory = shared_daemon_socket_directory()?;
    #[cfg(target_os = "linux")]
    prepare_user_runtime_directory()?;
    prepare_private_directory(&directory)?;
    Ok(directory)
}

/// Prepare process-lifetime coordination storage, independent of command settings.
#[cfg(target_os = "linux")]
pub fn prepare_user_runtime_directory() -> io::Result<PathBuf> {
    // The user manager owns this lifetime. Do not silently fall back to
    // disposable scratch or accept a command-controlled XDG_RUNTIME_DIR.
    let parent = user_runtime_directory();
    validate_private_directory(&parent)?;
    let directory = parent.join("codex-1337");
    prepare_private_directory(&directory)?;
    Ok(directory)
}

/// Protect both generations while older supervisors/servers may still be alive.
/// New listeners use only the runtime directory; the legacy root is a mask, not storage.
#[cfg(target_os = "linux")]
pub fn prepare_shared_daemon_socket_mask_directories() -> io::Result<[PathBuf; 2]> {
    let current = prepare_shared_daemon_socket_directory()?;
    let legacy = legacy_socket_directory()?;
    prepare_private_directory(&legacy)?;
    Ok([current, legacy])
}

fn prepare_private_directory(directory: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error),
    }
    validate_private_directory(directory)
}

fn validate_private_directory(directory: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(directory).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("Codex runtime directory {}: {error}", directory.display()),
        )
    })?;
    if !metadata.is_dir()
        || metadata.uid() != unsafe { libc::geteuid() }
        || metadata.mode() & 0o777 != 0o700
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "Codex runtime directory {} must be user-owned with mode 0700 and not a symlink",
                directory.display()
            ),
        ));
    }
    Ok(())
}
