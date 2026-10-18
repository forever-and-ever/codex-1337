//! A deliberately local adapter for Jira-at-home's filesystem format. No MCP binding,
//! upsert, or second database. The checksum check is optimistic: external writers do
//! not honor our process-local lock and can still race a checked mutation.

use codex_app_server_protocol::JiraAction;
use codex_app_server_protocol::JiraCategory;
use codex_app_server_protocol::JiraIssue;
use codex_app_server_protocol::JiraKey;
use codex_app_server_protocol::JiraResult;
use codex_app_server_protocol::JiraRevision;
use codex_app_server_protocol::ThreadJiraResponse;
#[cfg(target_os = "linux")]
use codex_uds::xdg_state_directory;
use codex_utils_path_uri::LegacyAppPathString;
use sha2::Digest;
use sha2::Sha256;
use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::fs;
use std::io::Read;
use std::io::Write;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Mutex;

const MAX_BODY_BYTES: usize = 1024 * 1024;
static MUTATIONS: Mutex<()> = Mutex::new(());

mod scope;

pub(crate) fn execute(
    cwd: &Path,
    expected_scope: Option<&str>,
    action: JiraAction,
) -> Result<ThreadJiraResponse, String> {
    let _lease = MUTATIONS.lock().map_err(|_| "Issue storage lock failed")?;
    let state = durable_state_dir().ok_or("Jira-at-home has no durable state directory on this server platform; /tmp fallback is deliberately refused")?;
    if expected_scope.is_none() && !matches!(action, JiraAction::List { cursor: None, .. }) {
        return Err("Open the issue list before reading or changing issues".into());
    }
    let scope = scope::Scope::open(cwd, &state, expected_scope)?;
    let result = match action {
        JiraAction::List {
            cursor,
            limit,
            recursive,
        } => scope.list(cursor.as_deref(), limit, recursive)?,
        action => {
            let project = match &action {
                JiraAction::Read { key } | JiraAction::Save { key, .. } => key.project.as_ref(),
                JiraAction::Delete { revision } => revision.key.project.as_ref(),
                JiraAction::List { .. } => unreachable!("lists are handled by the browsing scope"),
            };
            scope.store(project)?.execute(action)?
        }
    };
    Ok(ThreadJiraResponse {
        project: LegacyAppPathString::from_string(scope.cwd.display().to_string()),
        scope: scope.token,
        result,
    })
}

fn durable_state_dir() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        xdg_state_directory()
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

/// Parity boundary with Jira-at-home `ProjectLayout::bind`. Git administrative paths
/// may legitimately leave the checkout; issue storage paths may never follow links.
fn project_identity(cwd: &Path) -> std::io::Result<PathBuf> {
    let canonical = cwd.canonicalize()?;
    let anchor = if canonical.is_dir() {
        canonical
    } else {
        canonical
            .parent()
            .ok_or_else(|| std::io::Error::other("Project has no parent"))?
            .to_path_buf()
    };
    for checkout in anchor.ancestors() {
        let marker = checkout.join(".git");
        if !marker.exists() {
            continue;
        }
        let git_dir = if marker.is_dir() {
            marker.canonicalize()?
        } else {
            let raw = fs::read_to_string(&marker)?;
            let value = raw
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .and_then(|line| line.strip_prefix("gitdir:"))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| std::io::Error::other("Malformed Git indirection"))?;
            checkout.join(value).canonicalize()?
        };
        let common = match fs::read_to_string(git_dir.join("commondir")) {
            Ok(raw) => git_dir.join(raw.trim()).canonicalize()?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => git_dir,
            Err(error) => return Err(error),
        };
        return if common.file_name() == Some(OsStr::new(".git")) {
            common
                .parent()
                .map(Path::to_path_buf)
                .ok_or_else(|| std::io::Error::other("Git common dir has no parent"))
        } else {
            Ok(common)
        };
    }
    Ok(anchor)
}

fn segment(value: &str) -> Result<(), String> {
    if value.is_empty()
        || value.split('-').any(str::is_empty)
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
    {
        return Err(
            "Slug must use lowercase ASCII letters, digits and single hyphen separators".into(),
        );
    }
    Ok(())
}

/// Check each existing ancestor, not merely the final file. This refuses all
/// symlink entries, including dangling links, before creating or reading paths.
fn checked(path: &Path) -> Result<(), String> {
    for ancestor in path.ancestors() {
        match fs::symlink_metadata(ancestor) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(format!(
                    "Issue path contains a symlink: {}",
                    ancestor.display()
                ));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

struct Store {
    root: PathBuf,
    project: Option<LegacyAppPathString>,
}

#[derive(Debug)]
struct IssueLocation {
    key: JiraKey,
    path: PathBuf,
    epic: Option<String>,
}

impl Store {
    fn catalog(&self) -> Result<BTreeMap<String, IssueLocation>, String> {
        checked(&self.root)?;
        let mut issues = BTreeMap::new();
        for category in [JiraCategory::Bug, JiraCategory::Feature] {
            let dir = self.root.join(category.as_str());
            checked(&dir)?;
            let entries = match fs::read_dir(&dir) {
                Ok(entries) => entries,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.to_string()),
            };
            for entry in entries {
                let entry = entry.map_err(|error| error.to_string())?;
                let path = entry.path();
                checked(&path)?;
                if entry
                    .file_type()
                    .map_err(|error| error.to_string())?
                    .is_dir()
                {
                    let epic = entry
                        .file_name()
                        .into_string()
                        .map_err(|_| "Epic is not UTF-8")?;
                    segment(&epic)?;
                    for child in fs::read_dir(&path).map_err(|error| error.to_string())? {
                        let child = child.map_err(|error| error.to_string())?;
                        self.catalog_file(
                            &mut issues,
                            &category,
                            child.path(),
                            Some(epic.clone()),
                        )?;
                    }
                } else {
                    self.catalog_file(&mut issues, &category, path, /*epic*/ None)?;
                }
            }
        }
        Ok(issues)
    }

    fn catalog_file(
        &self,
        issues: &mut BTreeMap<String, IssueLocation>,
        category: &JiraCategory,
        path: PathBuf,
        epic: Option<String>,
    ) -> Result<(), String> {
        checked(&path)?;
        if !fs::symlink_metadata(&path)
            .map_err(|error| error.to_string())?
            .is_file()
        {
            return Err(format!("Malformed issue entry: {}", path.display()));
        }
        if path.extension() != Some(OsStr::new("md")) {
            return Ok(());
        }
        let slug = path
            .file_stem()
            .and_then(OsStr::to_str)
            .ok_or("Issue slug is not UTF-8")?
            .to_owned();
        segment(&slug)?;
        let key = JiraKey {
            project: self.project.clone(),
            category: category.clone(),
            slug,
        };
        let name = format!("{}/{}", category.as_str(), key.slug);
        if issues
            .insert(name.clone(), IssueLocation { key, path, epic })
            .is_some()
        {
            return Err(format!(
                "Ambiguous duplicate issue {name}; repair the store before continuing"
            ));
        }
        Ok(())
    }

    fn read(&self, key: &JiraKey) -> Result<JiraIssue, String> {
        if key.project != self.project {
            return Err("Issue project cannot be changed by an edit".into());
        }
        segment(&key.slug)?;
        let name = format!("{}/{}", key.category.as_str(), key.slug);
        let location = self
            .catalog()?
            .remove(&name)
            .ok_or_else(|| format!("Issue {name} no longer exists"))?;
        let mut bytes = Vec::new();
        fs::File::open(location.path)
            .map_err(|error| error.to_string())?
            .take((MAX_BODY_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|error| error.to_string())?;
        if bytes.len() > MAX_BODY_BYTES {
            return Err(
                "Issue exceeds the 1 MiB browser limit; edit it in the store directly".into(),
            );
        }
        let body = String::from_utf8(bytes).map_err(|error| error.to_string())?;
        let revision = JiraRevision {
            key: key.clone(),
            epic: location.epic,
            version: format!("{:x}", Sha256::digest(body.as_bytes())),
        };
        Ok(JiraIssue { revision, body })
    }

    fn guard(&self, revision: &JiraRevision) -> Result<(), String> {
        let current = self.read(&revision.key)?;
        if &current.revision != revision {
            return Err("Issue changed since it was read; draft retained. Reopen it before saving or deleting".into());
        }
        Ok(())
    }

    fn path(&self, key: &JiraKey, epic: Option<&str>) -> Result<PathBuf, String> {
        if key.project != self.project {
            return Err("Issue project cannot be changed by an edit".into());
        }
        segment(&key.slug)?;
        let mut dir = self.root.join(key.category.as_str());
        if let Some(epic) = epic {
            segment(epic)?;
            dir.push(epic);
        }
        let path = dir.join(format!("{}.md", key.slug));
        checked(&path)?;
        Ok(path)
    }

    fn execute(&self, action: JiraAction) -> Result<JiraResult, String> {
        match action {
            JiraAction::List { .. } => unreachable!("lists are handled by the browsing scope"),
            JiraAction::Read { key } => Ok(JiraResult::Issue {
                issue: self.read(&key)?,
            }),
            JiraAction::Delete { revision } => {
                self.guard(&revision)?;
                fs::remove_file(self.path(&revision.key, revision.epic.as_deref())?)
                    .map_err(|error| error.to_string())?;
                Ok(JiraResult::Deleted)
            }
            JiraAction::Save {
                previous,
                key,
                body,
            } => {
                if body.trim().is_empty() {
                    return Err("Issue body must not be empty".into());
                }
                if body.len() > MAX_BODY_BYTES {
                    return Err("Issue body exceeds the 1 MiB browser limit; nothing saved".into());
                }
                if let Some(previous) = &previous {
                    self.guard(previous)?;
                }
                let epic = previous
                    .as_ref()
                    .and_then(|previous| previous.epic.as_deref());
                let destination = self.path(&key, epic)?;
                let same_key = previous
                    .as_ref()
                    .is_some_and(|previous| previous.key == key);
                let name = format!("{}/{}", key.category.as_str(), key.slug);
                if !same_key && self.catalog()?.contains_key(&name) {
                    return Err(format!("Issue {name} already exists; choose another slug"));
                }
                let directory = destination
                    .parent()
                    .ok_or("Issue destination has no parent")?;
                fs::create_dir_all(directory).map_err(|error| error.to_string())?;
                checked(directory)?;
                let mut staged = tempfile::NamedTempFile::new_in(&self.root)
                    .map_err(|error| error.to_string())?;
                staged
                    .write_all(body.as_bytes())
                    .and_then(|()| staged.as_file().sync_all())
                    .map_err(|error| error.to_string())?;
                // Recheck after staging; no claim of CAS against external writers.
                if let Some(previous) = &previous {
                    self.guard(previous)?;
                }
                if same_key {
                    staged
                        .persist(&destination)
                        .map_err(|error| error.to_string())?;
                } else {
                    if self.catalog()?.contains_key(&name) {
                        return Err(format!("Issue {name} already exists; choose another slug"));
                    }
                    staged
                        .persist_noclobber(&destination)
                        .map_err(|error| error.to_string())?;
                    if let Some(previous) = previous {
                        if let Err(error) = self.guard(&previous) {
                            return Err(format!(
                                "New issue saved, but the original changed before removal. Both retained; inspect before retrying. {error}"
                            ));
                        }
                        let old = self.path(&previous.key, previous.epic.as_deref())?;
                        if let Err(error) = fs::remove_file(old) {
                            return Err(format!(
                                "New issue saved, but old issue could not be removed: {error}. Inspect both paths before retrying"
                            ));
                        }
                    }
                }
                Ok(JiraResult::Issue {
                    issue: self.read(&key)?,
                })
            }
        }
    }
}

#[cfg(test)]
#[path = "jira_store_tests.rs"]
mod tests;
