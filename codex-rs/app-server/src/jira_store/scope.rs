//! Browse stored namespaces, not checkout trees. A selector remains bound to the
//! server cwd; neither a stale view nor a forged key may move a mutation outside it.

use super::BTreeMap;
use super::Digest;
use super::JiraKey;
use super::JiraResult;
use super::LegacyAppPathString;
use super::Path;
use super::PathBuf;
use super::Sha256;
use super::Store;
use super::checked;
use super::fs;
use super::project_identity;
use std::path::Component;

pub(super) struct Scope {
    pub(super) cwd: PathBuf,
    pub(super) token: String,
    projects: PathBuf,
    default_project: PathBuf,
}

impl Scope {
    pub(super) fn open(cwd: &Path, state: &Path, expected: Option<&str>) -> Result<Self, String> {
        let default_project = project_identity(cwd).map_err(|error| error.to_string())?;
        let cwd = cwd.canonicalize().map_err(|error| error.to_string())?;
        let _ = cwd.to_str().ok_or("Browsing directory is not UTF-8")?;
        let mut hash = Sha256::new();
        // Old servers ignore project selectors. Reject their cached scope rather
        // than accidentally applying a descendant edit to the default project.
        hash.update(b"jira-project-scope-v2\0");
        hash.update(cwd.as_os_str().as_encoded_bytes());
        hash.update([0]);
        hash.update(default_project.as_os_str().as_encoded_bytes());
        let token = format!("{:x}", hash.finalize());
        if expected.is_some_and(|expected| expected != token) {
            return Err(
                "Conversation project changed; nothing read or saved. Close and reopen /jira."
                    .into(),
            );
        }
        Ok(Self {
            cwd,
            token,
            projects: state.join("jira_at_home/projects"),
            default_project,
        })
    }

    fn namespace(&self, project: &Path) -> PathBuf {
        let mut namespace = self.projects.clone();
        for component in project.components() {
            match component {
                Component::Normal(part) => namespace.push(part),
                Component::Prefix(prefix) => namespace.push(prefix.as_os_str()),
                Component::RootDir | Component::CurDir | Component::ParentDir => {}
            }
        }
        namespace
    }

    pub(super) fn store(&self, project: Option<&LegacyAppPathString>) -> Result<Store, String> {
        let path = match project {
            Some(project) => {
                let path = Path::new(project.as_str());
                if !path.is_absolute()
                    || !path.starts_with(&self.cwd)
                    || path.components().any(|component| {
                        matches!(component, Component::CurDir | Component::ParentDir)
                    })
                {
                    return Err("Issue project is outside the browsing directory".into());
                }
                path
            }
            None => &self.default_project,
        };
        Ok(Store {
            root: self.namespace(path).join("issues"),
            project: project.cloned(),
        })
    }

    pub(super) fn list(
        &self,
        cursor: Option<&str>,
        limit: Option<u32>,
        recursive: bool,
    ) -> Result<JiraResult, String> {
        let default = self.store(/*project*/ None)?;
        let mut all: BTreeMap<String, JiraKey> = default
            .catalog()?
            .into_iter()
            .map(|(name, location)| (format!("\0{name}"), location.key))
            .collect();
        if recursive {
            let prefix = self.namespace(&self.cwd);
            let mut pending = vec![prefix.clone()];
            while let Some(namespace) = pending.pop() {
                checked(&namespace)?;
                let entries = match fs::read_dir(&namespace) {
                    Ok(entries) => entries,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(error.to_string()),
                };
                for entry in entries {
                    let entry = entry.map_err(|error| error.to_string())?;
                    // Never follow symlinks out of the state tree. Files (including
                    // issue bodies) are not read by project discovery.
                    if entry
                        .file_type()
                        .map_err(|error| error.to_string())?
                        .is_dir()
                    {
                        pending.push(entry.path());
                    }
                }
                let root = namespace.join("issues");
                if root == default.root || !root.is_dir() {
                    continue;
                }
                let relative = namespace
                    .strip_prefix(&prefix)
                    .map_err(|error| error.to_string())?;
                let path = self.cwd.join(relative);
                let project = LegacyAppPathString::from_string(
                    path.to_str()
                        .ok_or("Stored project namespace is not UTF-8")?,
                );
                for (name, location) in self.store(Some(&project))?.catalog()? {
                    let _ = all.insert(format!("{}\0{name}", project.as_str()), location.key);
                }
            }
        }
        let limit = limit
            .unwrap_or(/*default*/ 100)
            .clamp(/*min*/ 1, /*max*/ 100) as usize;
        let mut remaining = all
            .into_iter()
            .filter(|(key, _)| cursor.is_none_or(|cursor| key.as_str() > cursor));
        let page: Vec<_> = remaining.by_ref().take(limit).collect();
        let next_cursor = if remaining.next().is_some() {
            page.last().map(|(key, _)| key.clone())
        } else {
            None
        };
        Ok(JiraResult::List {
            data: page.into_iter().map(|(_, key)| key).collect(),
            next_cursor,
        })
    }
}
