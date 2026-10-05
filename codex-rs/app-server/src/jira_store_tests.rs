use super::*;
use pretty_assertions::assert_eq;

#[test]
fn project_identity_matches_plain_linked_and_non_dot_git_store_layout() -> std::io::Result<()> {
    let fixture = tempfile::tempdir()?;
    let plain = fixture.path().join("plain");
    fs::create_dir(&plain)?;
    assert_eq!(project_identity(&plain)?, plain.canonicalize()?);
    let primary = fixture.path().join("primary");
    let admin = primary.join(".git");
    let gitdir = admin.join("worktrees/linked");
    let linked = fixture.path().join("linked");
    fs::create_dir_all(&gitdir)?;
    fs::create_dir(&linked)?;
    fs::write(linked.join(".git"), format!("gitdir: {}\n", gitdir.display()))?;
    fs::write(gitdir.join("commondir"), "../..\n")?;
    assert_eq!(project_identity(&linked)?, primary.canonicalize()?);
    let other_admin = fixture.path().join("administration");
    fs::rename(&admin, &other_admin)?;
    fs::write(linked.join(".git"), format!("gitdir: {}\n", other_admin.join("worktrees/linked").display()))?;
    assert_eq!(project_identity(&linked)?, other_admin.canonicalize()?);
    Ok(())
}

#[test]
fn mutations_preserve_epic_and_bytes_block_collisions_and_stale_revisions() -> Result<(), String> {
    let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
    let store = Store { root: fixture.path().join("issues"), scope: "fixture".into() };
    let epic = store.root.join("bug/latency");
    fs::create_dir_all(&epic).map_err(|error| error.to_string())?;
    let key = JiraKey { category: JiraCategory::Bug, slug: "slow-input".into() };
    let body = "freeform\n\n# Not a synthesized title\nπ\n";
    fs::write(epic.join("slow-input.md"), body).map_err(|error| error.to_string())?;
    let original = store.read(&key)?;
    assert_eq!((&original.body, original.revision.epic.as_deref()), (&body.to_owned(), Some("latency")));
    assert!(store.execute(JiraAction::Save { previous: None, key: key.clone(), body: "clobber".into() }).unwrap_err().contains("already exists"));
    let renamed = JiraKey { category: JiraCategory::Feature, slug: "fast-input".into() };
    store.execute(JiraAction::Save { previous: Some(original.revision.clone()), key: renamed.clone(), body: body.into() })?;
    let moved = store.read(&renamed)?;
    assert_eq!((&moved.body, moved.revision.epic.as_deref()), (&original.body, Some("latency")));
    assert!(!epic.join("slow-input.md").exists());
    let path = store.path(&renamed, Some("latency"))?;
    fs::write(&path, "external edit").map_err(|error| error.to_string())?;
    assert!(store.execute(JiraAction::Save { previous: Some(moved.revision.clone()), key: renamed.clone(), body: "stale".into() }).unwrap_err().contains("changed"));
    assert!(store.execute(JiraAction::Delete { revision: moved.revision }).unwrap_err().contains("changed"));
    assert_eq!(store.read(&renamed)?.body, "external edit");
    let current = store.read(&renamed)?;
    store.execute(JiraAction::Delete { revision: current.revision })?;
    assert!(path.parent().is_some_and(Path::exists));
    assert!(!path.exists());
    // Category-wide uniqueness includes root/epic entries, but not other categories.
    store.execute(JiraAction::Save { previous: None, key: key.clone(), body: body.into() })?;
    let other = JiraKey { category: JiraCategory::Feature, slug: key.slug.clone() };
    store.execute(JiraAction::Save { previous: None, key: other.clone(), body: body.into() })?;
    fs::write(epic.join("slow-input.md"), "duplicate").map_err(|error| error.to_string())?;
    assert!(store.catalog().unwrap_err().contains("Ambiguous duplicate"));
    let other_project = fixture.path().join("other-project");
    fs::create_dir(&other_project).map_err(|error| error.to_string())?;
    let first = Store::open(fixture.path(), fixture.path(), /*expected_scope*/ None)?;
    let changed = Store::open(&other_project, fixture.path(), Some(&first.scope));
    assert!(changed.err().is_some_and(|error| error.contains("project changed")));
    // Scope rejection precedes mkdir/save; another project never receives the draft.
    assert!(!fixture.path().join("jira_at_home").exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn issue_symlinks_are_refused_without_touching_the_target() -> Result<(), String> {
    let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
    let store = Store { root: fixture.path().join("issues"), scope: "fixture".into() };
    let dir = store.root.join("bug");
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let target = fixture.path().join("unrelated.md");
    fs::write(&target, "unrelated").map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&target, dir.join("escape.md")).map_err(|error| error.to_string())?;
    let key = JiraKey { category: JiraCategory::Bug, slug: "escape".into() };
    assert!(store.read(&key).unwrap_err().contains("symlink"));
    assert!(store.execute(JiraAction::Save { previous: None, key, body: "clobber".into() }).unwrap_err().contains("symlink"));
    assert_eq!(fs::read_to_string(target).map_err(|error| error.to_string())?, "unrelated");
    Ok(())
}
