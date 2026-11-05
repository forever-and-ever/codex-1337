//! Folds filesystem rules repeated under every workspace root into one `:workspace_roots`
//! rule, so the environment context lists the roots once instead of once per rule.

use super::ManagedFileSystemContext;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::FileSystemSpecialPath;
use codex_protocol::permissions::PROTECTED_METADATA_PATH_NAMES;
use codex_utils_path_uri::PathUri;

impl ManagedFileSystemContext {
    /// Folds each root's own rule and its protected metadata rules when every workspace root
    /// carries them with one access mode. The rendering shows only paths and access, so the
    /// fold loses nothing a model could read; a single root keeps its concrete paths.
    pub(super) fn fold_workspace_roots(mut self, workspace_roots: &[PathUri]) -> Self {
        if let Self::Restricted { entries, .. } = &mut self
            && workspace_roots.len() > 1
        {
            for subpath in
                std::iter::once(None).chain(PROTECTED_METADATA_PATH_NAMES.iter().copied().map(Some))
            {
                fold(entries, workspace_roots, subpath);
            }
        }
        self
    }
}

fn fold(
    entries: &mut Vec<FileSystemSandboxEntry>,
    workspace_roots: &[PathUri],
    subpath: Option<&str>,
) {
    let Some(targets) = workspace_roots
        .iter()
        .map(|root| match subpath {
            Some(subpath) => root.join_descendant(subpath).ok(),
            None => Some(root.clone()),
        })
        .collect::<Option<Vec<_>>>()
    else {
        return;
    };
    let target_of = |entry: &FileSystemSandboxEntry| match &entry.path {
        FileSystemPath::Path { path } => targets.iter().position(|target| target == path),
        FileSystemPath::GlobPattern { .. } | FileSystemPath::Special { .. } => None,
    };
    let Some(first) = entries.iter().position(|entry| target_of(entry).is_some()) else {
        return;
    };
    let access = entries[first].access;
    let mut covered = vec![false; targets.len()];
    for entry in entries.iter() {
        if let Some(target) = target_of(entry) {
            if entry.access != access
                || entry.missing_path_behavior != entries[first].missing_path_behavior
            {
                return;
            }
            covered[target] = true;
        }
    }
    if covered.contains(&false) {
        return;
    }
    let folded = FileSystemSandboxEntry {
        path: FileSystemPath::Special {
            value: FileSystemSpecialPath::project_roots(subpath.map(str::to_string)),
        },
        access,
        missing_path_behavior: entries[first].missing_path_behavior,
    };
    entries.retain(|entry| target_of(entry).is_none());
    entries.insert(first, folded);
}
