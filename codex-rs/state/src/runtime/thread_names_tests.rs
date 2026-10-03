use super::*;
use crate::runtime::test_support::test_thread_metadata;
use crate::runtime::test_support::unique_temp_dir;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn root_name_claim_race_and_archive_lifecycle() -> anyhow::Result<()> {
    let home = scopeguard::guard(unique_temp_dir(), |home| {
        let _ = std::fs::remove_dir_all(home);
    });
    let config = crate::SqliteConfig::new_for_testing(home.as_path().abs());
    let first = StateRuntime::init(config.clone(), "test-provider".to_string()).await?;
    let second = StateRuntime::init(config, "test-provider".to_string()).await?;
    first.import_legacy_thread_names(&[]).await?;
    let left = ThreadId::new();
    let right = ThreadId::new();
    for id in [left, right] {
        first
            .upsert_thread(&test_thread_metadata(
                home.as_path(),
                id,
                home.to_path_buf(),
            ))
            .await?;
    }
    // Different runtime handles model separate servers. A preflight availability check alone
    // cannot serialize these writers; only the persisted partial unique index can do so.
    let (left_result, right_result) = tokio::join!(
        first.update_thread_name(left, Some("  Siege  ")),
        second.update_thread_name(right, Some("siege")),
    );
    assert_eq!(
        usize::from(left_result.is_ok()) + usize::from(right_result.is_ok()),
        1
    );
    let (owner, contender) = if left_result.is_ok() {
        (left, right)
    } else {
        (right, left)
    };
    let owner_metadata = first.get_thread(owner).await?.expect("owner");
    assert_eq!(
        first.find_thread_name_owners(" SIEGE ").await?,
        vec![owner_metadata.clone()]
    );

    let mut family = test_thread_metadata(home.as_path(), ThreadId::new(), home.to_path_buf());
    family.source = r#"{"subagent":{"other":"guardian"}}"#.to_string();
    family.name = Some("siege".to_string());
    first.upsert_thread(&family).await?;
    first
        .mark_archived(owner, &owner_metadata.rollout_path, Utc::now())
        .await?;
    first.update_thread_name(contender, Some("siege")).await?;
    first
        .mark_unarchived(owner, &owner_metadata.rollout_path)
        .await?;
    let restored = first.get_thread(owner).await?.expect("restored");
    let restored_name = restored.name.as_deref().expect("restored name");
    assert!(restored_name.starts_with(&format!(
        "{}_unarchived_",
        owner_metadata.name.as_deref().expect("name")
    )));
    assert_eq!(
        first.find_thread_name_owners(restored_name).await?,
        vec![restored.clone()]
    );

    first
        .mark_archived(owner, &restored.rollout_path, Utc::now())
        .await?;
    first.mark_unarchived(owner, &restored.rollout_path).await?;
    assert_eq!(
        first.get_thread(owner).await?.expect("free restore").name,
        restored.name
    );
    // A compatibility projection cannot resurrect an explicitly cleared claim, even on retry.
    first.update_thread_name(owner, /*name*/ None).await?;
    first
        .import_legacy_thread_names(&[(owner, "stale legacy name".to_string())])
        .await?;
    assert_eq!(first.get_thread(owner).await?.expect("cleared").name, None);
    first.close().await;
    second.close().await;
    Ok(())
}
