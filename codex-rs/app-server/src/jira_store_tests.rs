use super::*;
use pretty_assertions::assert_eq;

#[test]
fn project_identity_matches_plain_linked_and_non_dot_git_store_layout() -> std::io::Result<()> {
    let fixture = tempfile::tempdir()?;
    let plain = fixture.path().join("plain");
    fs::create_dir(&plain)?;
    fs::create_dir(plain.join(".git"))?;
    assert_eq!(project_identity(&plain)?, plain.canonicalize()?);
    let primary = fixture.path().join("primary");
    let admin = primary.join(".git");
    let gitdir = admin.join("worktrees/linked");
    let linked = fixture.path().join("linked");
    fs::create_dir_all(&gitdir)?;
    fs::create_dir(&linked)?;
    fs::write(
        linked.join(".git"),
        format!("gitdir: {}\n", gitdir.display()),
    )?;
    fs::write(gitdir.join("commondir"), "../..\n")?;
    assert_eq!(project_identity(&linked)?, primary.canonicalize()?);
    let other_admin = fixture.path().join("administration");
    fs::rename(&admin, &other_admin)?;
    fs::write(
        linked.join(".git"),
        format!(
            "gitdir: {}\n",
            other_admin.join("worktrees/linked").display()
        ),
    )?;
    assert_eq!(project_identity(&linked)?, other_admin.canonicalize()?);
    Ok(())
}

#[test]
fn mutations_preserve_epic_and_bytes_block_collisions_and_stale_revisions() -> Result<(), String> {
    let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
    let store = Store {
        root: fixture.path().join("issues"),
        project: None,
    };
    let epic = store.root.join("bug/latency");
    fs::create_dir_all(&epic).map_err(|error| error.to_string())?;
    let key = JiraKey {
        project: None,
        category: JiraCategory::Bug,
        slug: "slow-input".into(),
    };
    let body = "freeform\n\n# Not a synthesized title\nπ\n";
    fs::write(epic.join("slow-input.md"), body).map_err(|error| error.to_string())?;
    let original = store.read(&key)?;
    assert_eq!(
        (&original.body, original.revision.epic.as_deref()),
        (&body.to_owned(), Some("latency"))
    );
    assert!(
        store
            .execute(JiraAction::Save {
                previous: None,
                key: key.clone(),
                body: "clobber".into()
            })
            .unwrap_err()
            .contains("already exists")
    );
    let renamed = JiraKey {
        project: None,
        category: JiraCategory::Feature,
        slug: "fast-input".into(),
    };
    store.execute(JiraAction::Save {
        previous: Some(original.revision.clone()),
        key: renamed.clone(),
        body: body.into(),
    })?;
    let moved = store.read(&renamed)?;
    assert_eq!(
        (&moved.body, moved.revision.epic.as_deref()),
        (&original.body, Some("latency"))
    );
    assert!(!epic.join("slow-input.md").exists());
    let path = store.path(&renamed, Some("latency"))?;
    fs::write(&path, "external edit").map_err(|error| error.to_string())?;
    assert!(
        store
            .execute(JiraAction::Save {
                previous: Some(moved.revision.clone()),
                key: renamed.clone(),
                body: "stale".into()
            })
            .unwrap_err()
            .contains("changed")
    );
    assert!(
        store
            .execute(JiraAction::Delete {
                revision: moved.revision
            })
            .unwrap_err()
            .contains("changed")
    );
    assert_eq!(store.read(&renamed)?.body, "external edit");
    let current = store.read(&renamed)?;
    store.execute(JiraAction::Delete {
        revision: current.revision,
    })?;
    assert!(path.parent().is_some_and(Path::exists));
    assert!(!path.exists());
    // Category-wide uniqueness includes root/epic entries, but not other categories.
    store.execute(JiraAction::Save {
        previous: None,
        key: key.clone(),
        body: body.into(),
    })?;
    let other = JiraKey {
        project: None,
        category: JiraCategory::Feature,
        slug: key.slug,
    };
    store.execute(JiraAction::Save {
        previous: None,
        key: other,
        body: body.into(),
    })?;
    fs::write(epic.join("slow-input.md"), "duplicate").map_err(|error| error.to_string())?;
    assert!(store.catalog().unwrap_err().contains("Ambiguous duplicate"));
    let other_project = fixture.path().join("other-project");
    fs::create_dir(&other_project).map_err(|error| error.to_string())?;
    let first = scope::Scope::open(fixture.path(), fixture.path(), /*expected*/ None)?;
    let changed = scope::Scope::open(&other_project, fixture.path(), Some(&first.token));
    assert!(
        changed
            .err()
            .is_some_and(|error| error.contains("project changed"))
    );
    // Scope rejection precedes mkdir/save; another project never receives the draft.
    assert!(!fixture.path().join("jira_at_home").exists());
    Ok(())
}

#[test]
fn recursive_namespaces_keep_paged_keys_and_mutations_in_their_project() -> Result<(), String> {
    // Source checkouts need not exist. A scope scans only the Jira namespace tree,
    // and identical issue slugs in different projects must never alias on mutation.
    let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
    let cwd = fixture.path().join("workspace");
    fs::create_dir_all(cwd.join(".git")).map_err(|error| error.to_string())?;
    let state = fixture.path().join("state");
    let scope = scope::Scope::open(&cwd, &state, /*expected*/ None)?;
    let default = scope.store(/*project*/ None)?;
    let child = LegacyAppPathString::from_string(cwd.join("child").display().to_string());
    let nested = LegacyAppPathString::from_string(cwd.join("child/nested").display().to_string());
    let keys: Vec<_> = [None, Some(child.clone()), Some(nested)]
        .into_iter()
        .map(|project| JiraKey {
            project,
            category: JiraCategory::Bug,
            slug: "same-slug".into(),
        })
        .collect();
    for key in &keys {
        scope
            .store(key.project.as_ref())?
            .execute(JiraAction::Save {
                previous: None,
                key: key.clone(),
                body: "original".into(),
            })?;
    }
    assert!(!cwd.join("child").exists());
    // A textual prefix would include workspace-other; a component prefix must not.
    let sibling = fixture.path().join("workspace-other");
    fs::create_dir_all(sibling.join(".git")).map_err(|error| error.to_string())?;
    let sibling_scope = scope::Scope::open(&sibling, &state, /*expected*/ None)?;
    let sibling_store = sibling_scope.store(/*project*/ None)?;
    sibling_store.execute(JiraAction::Save {
        previous: None,
        key: keys[0].clone(),
        body: "unrelated".into(),
    })?;
    #[cfg(unix)]
    std::os::unix::fs::symlink(
        sibling_store.root.parent().unwrap(),
        default.root.parent().unwrap().join("alias"),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        scope.list(
            /*cursor*/ None, /*limit*/ None, /*recursive*/ false
        )?,
        JiraResult::List {
            data: vec![keys[0].clone()],
            next_cursor: None,
        }
    );
    let mut found = Vec::new();
    let mut cursor = None;
    loop {
        let JiraResult::List { data, next_cursor } =
            scope.list(cursor.as_deref(), Some(2), /*recursive*/ true)?
        else {
            unreachable!()
        };
        found.extend(data);
        if next_cursor.is_none() {
            break;
        }
        cursor = next_cursor;
    }
    assert_eq!(found, keys);
    let child_store = scope.store(Some(&child))?;
    let old = child_store.read(&keys[1])?;
    assert!(
        child_store
            .execute(JiraAction::Save {
                previous: Some(old.revision.clone()),
                key: keys[0].clone(),
                body: "wrong project".into()
            })
            .is_err()
    );
    child_store.execute(JiraAction::Delete {
        revision: old.revision,
    })?;
    assert_eq!(default.read(&keys[0])?.body, "original");
    assert_eq!(sibling_store.read(&keys[0])?.body, "unrelated");
    assert!(
        scope
            .store(Some(&LegacyAppPathString::from_string(
                sibling.display().to_string()
            )))
            .is_err()
    );
    let escape =
        LegacyAppPathString::from_string(format!("{}/child/../../workspace-other", cwd.display()));
    assert!(scope.store(Some(&escape)).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn issue_symlinks_are_refused_without_touching_the_target() -> Result<(), String> {
    let fixture = tempfile::tempdir().map_err(|error| error.to_string())?;
    let store = Store {
        root: fixture.path().join("issues"),
        project: None,
    };
    let dir = store.root.join("bug");
    fs::create_dir_all(&dir).map_err(|error| error.to_string())?;
    let target = fixture.path().join("unrelated.md");
    fs::write(&target, "unrelated").map_err(|error| error.to_string())?;
    std::os::unix::fs::symlink(&target, dir.join("escape.md"))
        .map_err(|error| error.to_string())?;
    let key = JiraKey {
        project: None,
        category: JiraCategory::Bug,
        slug: "escape".into(),
    };
    assert!(store.read(&key).unwrap_err().contains("symlink"));
    assert!(
        store
            .execute(JiraAction::Save {
                previous: None,
                key,
                body: "clobber".into()
            })
            .unwrap_err()
            .contains("symlink")
    );
    assert_eq!(
        fs::read_to_string(target).map_err(|error| error.to_string())?,
        "unrelated"
    );
    Ok(())
}
