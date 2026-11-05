//! AWS credential helpers matter at the executor's home, not at arbitrary workspace roots.

use codex_utils_path_uri::PathConvention;
use std::borrow::Cow;
use std::ffi::OsStr;
use std::path::Path;

use codex_utils_absolute_path::AbsolutePathBuf;
use codex_utils_path_uri::PathUri;

use super::FileSystemAccessMode;
use super::FileSystemPath;
use super::FileSystemSandboxEntry;
use super::FileSystemSandboxPolicy;
use super::FileSystemSandboxPolicyContext;
use super::FileSystemSpecialPath;
use super::PROTECTED_METADATA_AWS_PATH_NAME;

pub fn protected_metadata_name_in_context(
    name: &str,
    root: &PathUri,
    context: &FileSystemSandboxPolicyContext<'_>,
) -> bool {
    !cfg!(target_os = "linux")
        || root.infer_path_convention() != Some(PathConvention::Posix)
        || name != PROTECTED_METADATA_AWS_PATH_NAME
        || context.user_home_dir == Some(root)
}

pub fn is_optional_aws_entry(entry: &FileSystemSandboxEntry) -> bool {
    cfg!(target_os = "linux")
        && entry.skips_missing_path()
        && match &entry.path {
            FileSystemPath::Path { path } => path
                .to_abs_path()
                .is_ok_and(|path| is_aws_path(path.as_path())),
            FileSystemPath::Special {
                value: FileSystemSpecialPath::ProjectRoots { subpath },
            } => subpath.as_deref() == Some(PROTECTED_METADATA_AWS_PATH_NAME),
            FileSystemPath::GlobPattern { .. } | FileSystemPath::Special { .. } => false,
        }
}

fn is_aws_path(path: &Path) -> bool {
    path.file_name() == Some(OsStr::new(PROTECTED_METADATA_AWS_PATH_NAME))
}

fn is_local_home(root: &Path) -> bool {
    let Some(home) = PathUri::from_host_native_path("~")
        .ok()
        .and_then(|home| home.to_abs_path().ok())
    else {
        return false;
    };
    root == home.as_path()
        || root
            .canonicalize()
            .ok()
            .zip(home.as_path().canonicalize().ok())
            .is_some_and(|(root, home)| root == home)
}

pub(super) fn protects_local_name(name: &str, root: &AbsolutePathBuf) -> bool {
    !cfg!(target_os = "linux")
        || name != PROTECTED_METADATA_AWS_PATH_NAME
        || is_local_home(root.as_path())
}

pub(super) fn protects_local_aws(path: &Path) -> bool {
    if !cfg!(target_os = "linux") {
        return path.is_dir();
    }
    path.parent().is_some_and(is_local_home)
        || std::fs::symlink_metadata(path).map_or_else(
            |error| error.kind() != std::io::ErrorKind::NotFound,
            |_| true,
        )
}

pub(super) fn retain_local_entry(entry: &FileSystemSandboxEntry, path: &AbsolutePathBuf) -> bool {
    !is_optional_aws_entry(entry) || protects_local_aws(path.as_path())
}

/// Local matching alone may inspect paths; symbolic/remote policy resolution must not.
pub(super) fn localize<'a>(
    policy: &'a FileSystemSandboxPolicy,
    context: &FileSystemSandboxPolicyContext<'_>,
) -> Cow<'a, FileSystemSandboxPolicy> {
    if !cfg!(target_os = "linux") || policy.has_full_disk_write_access() {
        return Cow::Borrowed(policy);
    }
    let mut scoped = policy.clone();
    scoped.entries.clear();
    for entry in &policy.entries {
        if !is_optional_aws_entry(entry) {
            scoped.entries.push(entry.clone());
            continue;
        }
        let isolated = FileSystemSandboxPolicy::restricted(vec![entry.clone()]);
        scoped
            .entries
            .extend(
                isolated
                    .resolved_entries(context)
                    .into_iter()
                    .filter_map(|(path, _)| {
                        let native = path.to_abs_path().ok()?;
                        protects_local_aws(native.as_path()).then(|| FileSystemSandboxEntry {
                            path: path.into(),
                            ..entry.clone()
                        })
                    }),
            );
    }
    let resolved = scoped.resolved_entries(context);
    for (root, access) in &resolved {
        if !access.can_write() {
            continue;
        }
        let Ok(path) = root.join_descendant(PROTECTED_METADATA_AWS_PATH_NAME) else {
            continue;
        };
        if path
            .to_abs_path()
            .is_ok_and(|path| protects_local_aws(path.as_path()))
            && !resolved.iter().any(|(existing, _)| existing == &path)
        {
            scoped
                .entries
                .push(FileSystemSandboxEntry::skip_missing_path(
                    path.into(),
                    FileSystemAccessMode::Read,
                ));
        }
    }
    if scoped == *policy {
        Cow::Borrowed(policy)
    } else {
        Cow::Owned(scoped)
    }
}

pub(crate) fn intersect_optional_metadata(
    path: FileSystemPath,
    access: FileSystemAccessMode,
    optional: bool,
    required: bool,
) -> FileSystemSandboxEntry {
    let conditional_aws = cfg!(target_os = "linux")
        && matches!(&path, FileSystemPath::Path { path } if path.to_abs_path().is_ok_and(|path| is_aws_path(path.as_path())));
    if optional
        && !required
        && (access == FileSystemAccessMode::Read
            || conditional_aws && access == FileSystemAccessMode::Write)
    {
        FileSystemSandboxEntry::skip_missing_path(path, FileSystemAccessMode::Read)
    } else {
        FileSystemSandboxEntry::new(path, access)
    }
}
