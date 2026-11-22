//! Verifies database failures preserve saved threads and report actionable diagnostics.

use anyhow::Result;
use app_test_support::TestAppServer;
use codex_app_server_protocol::ConfigWarningNotification;
use codex_app_server_protocol::ThreadStartParams;
use codex_state::SqliteConfig;
use codex_state::StateRuntime;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use std::time::Duration;
use tempfile::TempDir;
use tokio::time::timeout;

#[tokio::test]
async fn startup_reports_shared_and_fallback_recovery() -> Result<()> {
    for log_dir_is_configured in [false, true] {
        let home = TempDir::new()?;
        if log_dir_is_configured {
            std::fs::write(
                home.path().join("config.toml"),
                format!("log_dir = {:?}\n", home.path().join("log")),
            )?;
        }
        let sqlite = core_test_support::load_default_config_for_test(&home)
            .await
            .sqlite;
        let runtime = StateRuntime::init(sqlite.clone(), "openai".to_string()).await?;
        runtime.close().await;
        let memories_path = sqlite.memories_db_path();
        let pool = sqlite.open_read_write_pool(&memories_path).await?;
        // The schema and migrations remain readable; opening the database alone succeeds.
        sqlx::raw_sql(
            "CREATE TABLE sample(value INTEGER);
             INSERT INTO sample VALUES (NULL);
             PRAGMA writable_schema=ON;
             UPDATE sqlite_schema SET sql='CREATE TABLE sample(value INTEGER NOT NULL)' WHERE name='sample';
             PRAGMA writable_schema=OFF;",
        )
        .execute(&pool)
        .await?;
        pool.close().await;

        // Memories recover inside the pool opener; the unreadable goals header forces
        // the outer startup fallback and a retry after that first recovery.
        tokio::fs::write(sqlite.goals_db_path(), b"damaged goals database").await?;

        let read_timeout = Duration::from_secs(/*secs*/ 30);
        let mut server = TestAppServer::builder()
            .with_codex_home(sqlite.home())
            .build_initialized_with_timeout(read_timeout)
            .await?;
        let warning: ConfigWarningNotification =
            timeout(read_timeout, server.read_notification("configWarning")).await??;
        assert_eq!(warning.summary, "Codex rebuilt its local database");
        let details = warning.details.expect("recovery details");
        assert!(details.contains("Some database-only metadata may be unavailable"));
        let mut backups = std::fs::read_dir(sqlite.home().join("db-backups"))?
            .collect::<std::io::Result<Vec<_>>>()?;
        let memories_home = memories_path.parent().expect("memories directory");
        if memories_home != sqlite.home() {
            backups.extend(
                std::fs::read_dir(memories_home.join("db-backups"))?
                    .collect::<std::io::Result<Vec<_>>>()?,
            );
        }
        assert_eq!(backups.len(), 2);
        for backup in &backups {
            assert!(details.contains(&backup.path().display().to_string()));
        }
        assert!(details.contains(&sqlite.memories_db_path().display().to_string()));
        assert!(details.contains(&sqlite.goals_db_path().display().to_string()));
        let backup_dir = backups
            .iter()
            .map(std::fs::DirEntry::path)
            .find(|path| {
                path.join(memories_path.file_name().expect("memories filename"))
                    .exists()
            })
            .expect("preserved memories database");
        let backup_pool = sqlite
            .open_read_only_pool(
                &backup_dir.join(memories_path.file_name().expect("memories filename")),
                /*busy_timeout*/ None,
            )
            .await?;
        assert_eq!(
            sqlx::query_scalar::<_, Option<i64>>("SELECT value FROM sample")
                .fetch_all(&backup_pool)
                .await?,
            vec![None]
        );
        backup_pool.close().await;
        server.start_thread(ThreadStartParams::default()).await?;
        timeout(read_timeout, server.shutdown_gracefully()).await??;
        // The same outer fallback must not rebuild authoritative accepted-mail/user-input data.
        let damaged_queue = b"damaged authoritative queue";
        tokio::fs::write(sqlite.queue_db_path(), damaged_queue).await?;
        let blocked = TestAppServer::builder()
            .with_codex_home(sqlite.home())
            .build_initialized_with_timeout(read_timeout)
            .await;
        assert!(blocked.is_err());
        assert_eq!(
            tokio::fs::read(sqlite.queue_db_path()).await?,
            damaged_queue
        );
        let mut backup_roots = vec![sqlite.home(), memories_home];
        backup_roots.sort();
        backup_roots.dedup();
        let backup_count = backup_roots
            .into_iter()
            .map(|root| std::fs::read_dir(root.join("db-backups")).map(|entries| entries.count()))
            .collect::<std::io::Result<Vec<_>>>()?
            .into_iter()
            .sum::<usize>();
        assert_eq!(backup_count, backups.len());
    }
    Ok(())
}

#[tokio::test]
async fn state_recovery_restores_saved_threads() -> Result<()> {
    use codex_app_server_protocol::ThreadListParams;
    use codex_app_server_protocol::ThreadListResponse;
    use codex_app_server_protocol::ThreadReadParams;
    use codex_app_server_protocol::ThreadReadResponse;
    use codex_app_server_protocol::TurnStartParams;
    use codex_app_server_protocol::TurnStatus;
    use codex_app_server_protocol::UserInput;

    let home = TempDir::new()?;
    let mock =
        app_test_support::create_mock_responses_server_repeating_assistant("Saved reply").await;
    app_test_support::MockResponsesConfig::new(&mock.uri()).write(home.path())?;
    let sqlite = SqliteConfig::new_for_testing(home.path().canonicalize()?.as_path().abs());
    let read_timeout = Duration::from_secs(/*secs*/ 30);
    let mut server = TestAppServer::builder()
        .with_codex_home(sqlite.home())
        .build_initialized_with_timeout(read_timeout)
        .await?;
    let thread_id = server
        .start_thread(ThreadStartParams::default())
        .await?
        .thread
        .id;
    let completed = server
        .start_turn_and_wait_for_completion(TurnStartParams {
            thread_id: thread_id.clone(),
            input: vec![UserInput::Text {
                text: "Keep this conversation".to_string(),
                text_elements: Vec::new(),
            }],
            ..Default::default()
        })
        .await?;
    assert_eq!(completed.turn.status, TurnStatus::Completed);
    let request = server
        .send_thread_read_request(ThreadReadParams {
            thread_id: thread_id.clone(),
            include_turns: true,
        })
        .await?;
    let before: ThreadReadResponse = timeout(read_timeout, server.read_response(request)).await??;
    timeout(read_timeout, server.shutdown_gracefully()).await??;

    // Introduce corruption only after the owning process has closed its pools.
    let pool = sqlite.open_read_write_pool(&sqlite.state_db_path()).await?;
    sqlx::raw_sql(
        "CREATE TABLE sample(value INTEGER);
         INSERT INTO sample VALUES (NULL);
         PRAGMA writable_schema=ON;
         UPDATE sqlite_schema SET sql='CREATE TABLE sample(value INTEGER NOT NULL)' WHERE name='sample';
         PRAGMA writable_schema=OFF;",
    ).execute(&pool).await?;
    pool.close().await;

    let mut server = TestAppServer::builder()
        .with_codex_home(sqlite.home())
        .build_initialized_with_timeout(read_timeout)
        .await?;
    let request = server
        .send_thread_list_request(ThreadListParams {
            include_attached_threads: false,
            current_account_only: false,
            included_thread_ids: None,
            excluded_thread_ids: None,
            attached_to_thread_id: None,
            originators: None,
            cursor: None,
            limit: None,
            sort_key: None,
            sort_direction: None,
            model_providers: None,
            source_kinds: None,
            archived: None,
            section_id: None,
            project_id: None,
            cwd: None,
            use_state_db_only: true,
            search_term: None,
            parent_thread_id: None,
            ancestor_thread_id: None,
        })
        .await?;
    let listed: ThreadListResponse = timeout(read_timeout, server.read_response(request)).await??;
    assert_eq!(
        listed
            .data
            .iter()
            .map(|thread| thread.id.as_str())
            .collect::<Vec<_>>(),
        vec![thread_id.as_str()]
    );
    let request = server
        .send_thread_read_request(ThreadReadParams {
            thread_id,
            include_turns: true,
        })
        .await?;
    let after: ThreadReadResponse = timeout(read_timeout, server.read_response(request)).await??;
    assert_eq!(after.thread.turns, before.thread.turns);
    let warning: ConfigWarningNotification =
        timeout(read_timeout, server.read_notification("configWarning")).await??;
    assert_eq!(warning.summary, "Codex rebuilt its local database");
    let details = warning.details.expect("recovery details");
    assert!(details.contains("Some database-only metadata may be unavailable"));
    let backups = std::fs::read_dir(sqlite.home().join("db-backups"))?
        .collect::<std::io::Result<Vec<_>>>()?;
    assert_eq!(backups.len(), 1);
    assert!(details.contains(&backups[0].path().display().to_string()));
    assert!(
        backups[0]
            .path()
            .join(sqlite.state_db_path().file_name().expect("state filename"))
            .is_file()
    );
    timeout(read_timeout, server.shutdown_gracefully()).await??;
    Ok(())
}
