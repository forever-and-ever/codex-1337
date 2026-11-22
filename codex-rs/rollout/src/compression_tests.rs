use std::fs;
#[cfg(unix)]
use std::sync::atomic::Ordering;
use std::time::Duration;

use codex_protocol::ThreadId;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::SessionMeta;
use codex_protocol::protocol::SessionMetaLine;
use codex_protocol::protocol::SessionSource;
use codex_protocol::protocol::UserMessageEvent;
use pretty_assertions::assert_eq;
use tempfile::TempDir;
use uuid::Uuid;

use super::*;
use crate::RolloutItem;
use crate::RolloutLine;
use crate::RolloutRecorder;
use crate::RolloutSnippetMatcher;
use crate::append_rollout_item_to_path;
use crate::read_session_meta_line;
use crate::search_rollout_matches;
use std::collections::HashMap;

#[tokio::test]
async fn load_rollout_items_reads_compressed_rollout() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(1);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "hello compressed")?;
    compress_now(&rollout_path)?;

    let (items, loaded_thread_id, parse_errors) =
        RolloutRecorder::load_rollout_items(&rollout_path).await?;

    assert_eq!(loaded_thread_id, Some(thread_id));
    assert_eq!(parse_errors, 0);
    assert_eq!(items.len(), 2);
    assert!(!rollout_path.exists());
    assert!(path::compressed_rollout_path(&rollout_path).exists());
    Ok(())
}

#[tokio::test]
async fn full_history_load_preserves_records_and_errors_across_representations()
-> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(19);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&path, thread_id, "message with unicode: π")?;
    let original = fs::read_to_string(&path)?;
    let expected = RolloutRecorder::load_rollout_items(&path).await?;
    let contents = format!(
        " \r\ninvalid JSON\n{{}}\n{}",
        original.trim_end().replace('\n', "\r\n")
    );
    fs::write(&path, &contents)?;
    let expected = (expected.0, expected.1, 2);
    assert_eq!(
        serde_json::to_value(RolloutRecorder::load_rollout_items(&path).await?)?,
        serde_json::to_value(&expected)?
    );
    compress_now(&path)?;
    assert_eq!(
        serde_json::to_value(RolloutRecorder::load_rollout_items(&path).await?)?,
        serde_json::to_value(&expected)?
    );

    for bytes in [b" \r\n\n".as_slice(), b"valid utf8\n\xff".as_slice()] {
        fs::write(&path, bytes)?;
        let plain = RolloutRecorder::load_rollout_items(&path)
            .await
            .unwrap_err();
        compress_now(&path)?;
        let compressed = RolloutRecorder::load_rollout_items(&path)
            .await
            .unwrap_err();
        assert_eq!(
            (compressed.kind(), compressed.to_string()),
            (plain.kind(), plain.to_string())
        );
    }
    Ok(())
}

#[tokio::test]
async fn read_session_meta_line_stops_before_invalid_utf8_tail() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(16);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "ignored tail")?;
    let mut contents = fs::read(&rollout_path)?;
    contents.resize(contents.len() + 64 * 1024, 0xff);
    fs::write(&rollout_path, contents)?;

    let session_meta = read_session_meta_line(&rollout_path).await?;

    assert_eq!(session_meta.meta.id, thread_id);
    Ok(())
}

#[tokio::test]
async fn read_session_meta_line_stops_before_invalid_utf8_compressed_tail() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(17);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "ignored compressed tail")?;
    let mut contents = fs::read(&rollout_path)?;
    contents.resize(contents.len() + 64 * 1024, 0xff);
    fs::write(&rollout_path, contents)?;
    compress_now(&rollout_path)?;

    let session_meta = read_session_meta_line(&rollout_path).await?;

    assert_eq!(session_meta.meta.id, thread_id);
    assert!(!rollout_path.exists());
    assert!(path::compressed_rollout_path(&rollout_path).exists());
    Ok(())
}

#[tokio::test]
async fn read_session_meta_line_preserves_pre_header_and_error_semantics() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(18);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "after metadata")?;
    let contents = fs::read_to_string(&rollout_path)?;
    let (session_meta, tail) = contents.split_once('\n').expect("session metadata line");
    let mut session_meta = serde_json::from_str::<serde_json::Value>(session_meta)?;
    session_meta["payload"]
        .as_object_mut()
        .expect("session metadata payload")
        .remove("session_id");
    let contents = format!("{session_meta}\n{tail}");
    let ignored_event = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "event_msg",
        "payload": {"type": "user_message", "message": "before metadata", "kind": "plain"}
    });
    fs::write(
        &rollout_path,
        format!("\nnot json\n{ignored_event}\n{contents}"),
    )?;

    let session_meta = read_session_meta_line(&rollout_path).await?;

    assert_eq!(session_meta.meta.id, thread_id);

    let response_item = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "response_item",
        "payload": {
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "before metadata"}]
        }
    });
    fs::write(&rollout_path, format!("{response_item}\n{contents}"))?;

    let error = read_session_meta_line(&rollout_path)
        .await
        .expect_err("response item before metadata should fail");

    assert_eq!(
        error.to_string(),
        format!(
            "rollout at {} does not start with session metadata",
            rollout_path.display()
        )
    );

    let unknown_history_mode = serde_json::json!({
        "timestamp": "2025-01-03T12:00:00Z",
        "type": "session_meta",
        "payload": {
            "session_id": uuid.to_string(),
            "id": uuid.to_string(),
            "timestamp": "2025-01-03T12:00:00Z",
            "cwd": ".",
            "originator": "test",
            "cli_version": "test",
            "source": "cli",
            "model_provider": "test-provider",
            "history_mode": "future",
        },
    });
    fs::write(&rollout_path, format!("{unknown_history_mode}\n{contents}"))?;

    let error = read_session_meta_line(&rollout_path)
        .await
        .expect_err("unknown history mode before valid metadata should fail");

    assert!(
        error
            .to_string()
            .starts_with("invalid session metadata history_mode:"),
        "{error}"
    );

    fs::write(&rollout_path, "\nnot json\n")?;

    let error = read_session_meta_line(&rollout_path)
        .await
        .expect_err("rollout without selected records should fail");

    assert_eq!(
        error.to_string(),
        format!("rollout at {} is empty", rollout_path.display())
    );
    Ok(())
}

#[test]
fn rollout_file_from_path_normalizes_compressed_file_names() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(7);
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    let compressed_path = path::compressed_rollout_path(&rollout_path);

    assert_eq!(
        RolloutFile::from_path(compressed_path.clone()),
        Some(RolloutFile {
            path: compressed_path,
            plain_file_name: format!("rollout-2025-01-03T12-00-00-{uuid}.jsonl"),
        })
    );
    Ok(())
}

#[test]
fn rollout_file_from_path_hides_compressed_sibling_when_plain_exists() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(8);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "plain wins")?;

    assert_eq!(
        RolloutFile::from_path(path::compressed_rollout_path(&rollout_path)),
        None
    );
    Ok(())
}

#[tokio::test]
async fn append_converts_a_single_frame_rollout_to_framed() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(2);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "hello before append")?;
    compress_now(&rollout_path)?;

    append_rollout_item_to_path(
        &rollout_path,
        &RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
            message: "hello after append".to_string(),
            ..Default::default()
        })),
    )
    .await?;

    // The append extends the framed encoding in place; no plain copy appears.
    assert!(!rollout_path.exists());
    assert!(crate::framed::is_framed(&std::fs::File::open(
        path::compressed_rollout_path(&rollout_path)
    )?)?);
    let (items, loaded_thread_id, parse_errors) =
        RolloutRecorder::load_rollout_items(&rollout_path).await?;
    assert_eq!(loaded_thread_id, Some(thread_id));
    assert_eq!(parse_errors, 0);
    assert_eq!(items.len(), 3);
    Ok(())
}

#[tokio::test]
async fn search_rollout_matches_uses_logical_path_for_compressed_rollout() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(15);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "targeted search term")?;
    compress_now(&rollout_path)?;

    let matches = search_rollout_matches(
        std::path::Path::new("missing-rg-for-test"),
        home.path(),
        /*archived*/ false,
        "search term",
    )
    .await?;

    assert_eq!(matches, std::collections::HashSet::from([rollout_path]));
    Ok(())
}

#[tokio::test]
async fn compressed_search_preserves_first_visible_snippet_and_no_match() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(22);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&path, thread_id, "an unrelated first message")?;
    let mut contents = fs::read(&path)?;
    contents.extend_from_slice(b"{malformed needle raw-only}\n");
    fs::write(&path, contents)?;
    for message in [
        "the first Needle [x] is visible",
        "the second needle is visible",
    ] {
        append_rollout_item_to_path(
            &path,
            &RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: message.to_string(),
                ..Default::default()
            })),
        )
        .await?;
    }

    for compressed in [false, true] {
        if compressed {
            compress_now(&path)?;
        }
        for (query, expected) in [
            ("NEEDLE", Some("the first Needle [x] is visible")),
            ("[x]", Some("the first Needle [x] is visible")),
            ("raw-only", None),
            ("absent", None),
        ] {
            assert_eq!(
                RolloutSnippetMatcher::new(query).first_match(&path).await?,
                expected.map(str::to_string),
                "compressed={compressed}, query={query}"
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn compressed_search_only_reports_stream_errors_before_a_match() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(23);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&path, thread_id, "needle before unreadable tail")?;
    let mut contents = fs::read(&path)?;
    contents.extend_from_slice(b"\xff\n");
    fs::write(&path, contents)?;
    compress_now(&path)?;
    assert_eq!(
        RolloutSnippetMatcher::new("needle")
            .first_match(&path)
            .await?,
        Some("needle before unreadable tail".to_owned())
    );
    assert_eq!(
        RolloutSnippetMatcher::new("absent")
            .first_match(&path)
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::InvalidData
    );
    Ok(())
}

#[tokio::test]
async fn compressed_search_stops_between_lines_when_cancelled() -> anyhow::Result<()> {
    struct NotifyOnDrop(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for NotifyOnDrop {
        fn drop(&mut self) {
            if let Some(sender) = self.0.take() {
                let _ = sender.send(());
            }
        }
    }
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(24);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&path, thread_id, "multiple lines are available")?;
    compress_now(&path)?;
    let reader = open_rollout_line_reader(&path).await?;
    let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = calls.clone();
    let (entered, waiting) = tokio::sync::oneshot::channel();
    let (release, blocked) = std::sync::mpsc::channel();
    let (finished, done) = tokio::sync::oneshot::channel();
    let guard = NotifyOnDrop(Some(finished));
    let mut entered = Some(entered);
    let handle = tokio::spawn(reader.find_map(move |_| {
        let _guard = &guard;
        seen.fetch_add(1, Ordering::SeqCst);
        if let Some(entered) = entered.take() {
            let _ = entered.send(());
            let _ = blocked.recv();
        }
        None::<()>
    }));
    waiting.await?;
    handle.abort();
    assert!(handle.await.unwrap_err().is_cancelled());
    release.send(())?;
    tokio::time::timeout(Duration::from_secs(5), done).await??;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    Ok(())
}

#[tokio::test]
async fn find_thread_path_by_id_handles_compressed_rollout_filenames() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(8);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let rollout_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid);
    write_rollout(&rollout_path, thread_id, "compressed filename lookup")?;
    compress_now(&rollout_path)?;
    let compressed_path = path::compressed_rollout_path(&rollout_path);

    for id in 1000..1064 {
        let unrelated_path = path::compressed_rollout_path(&rollout_path.with_file_name(format!(
            "rollout-2025-01-03T12-00-00-{}.jsonl",
            Uuid::from_u128(id)
        )));
        fs::write(unrelated_path, zstd::stream::encode_all(&b""[..], 0)?)?;
    }

    assert_eq!(
        crate::find_thread_path_by_id_str(
            home.path(),
            &uuid.to_string(),
            /*state_db_ctx*/ None
        )
        .await?,
        Some(compressed_path.clone())
    );
    assert_eq!(
        crate::find_rollout_path_by_rollout_id(home.path(), thread_id).await?,
        Some(compressed_path)
    );

    write_rollout(&rollout_path, thread_id, "plain filename lookup")?;
    assert_eq!(
        crate::find_thread_path_by_id_str(
            home.path(),
            &uuid.to_string(),
            /*state_db_ctx*/ None
        )
        .await?,
        Some(rollout_path.clone())
    );
    assert_eq!(
        crate::find_rollout_path_by_rollout_id(home.path(), thread_id).await?,
        Some(rollout_path.clone())
    );
    let missing_rollout_id = ThreadId::from_string(&Uuid::from_u128(2000).to_string())?;
    assert_eq!(
        crate::find_rollout_path_by_rollout_id(home.path(), missing_rollout_id).await?,
        None
    );
    let archived_id = ThreadId::new();
    let archived_path = home
        .path()
        .join(crate::ARCHIVED_SESSIONS_SUBDIR)
        .join(format!("rollout-2025-01-03T12-00-00-{archived_id}.jsonl"));
    write_rollout(&archived_path, archived_id, "archived original")?;
    compress_now(&archived_path)?;
    assert_eq!(
        crate::find_rollout_paths_by_rollout_ids(
            home.path(),
            &[thread_id, archived_id, missing_rollout_id],
        )
        .await,
        HashMap::from([
            (thread_id, rollout_path),
            (archived_id, compressed_rollout_path(&archived_path)),
        ])
    );
    assert_eq!(
        crate::find_thread_path_by_id_str(home.path(), "not-a-uuid", /*state_db_ctx*/ None).await?,
        None
    );
    Ok(())
}

#[tokio::test]
async fn find_thread_path_by_id_ignores_compression_temp_matches() -> anyhow::Result<()> {
    let home = TempDir::new()?;
    let uuid = Uuid::from_u128(9);
    let thread_id = ThreadId::from_string(&uuid.to_string())?;
    let temp_path = rollout_path(home.path(), "2025-01-03T12-00-00", uuid).with_file_name(format!(
        "rollout-2025-01-03T12-00-00-{uuid}.jsonl.zst.compress.1.0.tmp"
    ));
    write_rollout(&temp_path, thread_id, "temporary file should not resolve")?;

    assert_eq!(
        crate::find_thread_path_by_id_str(
            home.path(),
            &uuid.to_string(),
            /*state_db_ctx*/ None
        )
        .await?,
        None
    );
    Ok(())
}

fn rollout_path(home: &std::path::Path, ts: &str, uuid: Uuid) -> std::path::PathBuf {
    home.join("sessions/2025/01/03")
        .join(format!("rollout-{ts}-{uuid}.jsonl"))
}

fn write_rollout(path: &std::path::Path, thread_id: ThreadId, message: &str) -> anyhow::Result<()> {
    let parent = path.parent().expect("rollout path should have parent");
    fs::create_dir_all(parent)?;
    let session_meta_line = SessionMetaLine {
        meta: SessionMeta {
            account_scoped: false,
            attached_thread: false,
            creator_user_id: None,
            creator_account_id: None,
            session_id: thread_id.into(),
            id: thread_id,
            forked_from_id: None,
            forked_from_ordinal_exclusive: None,
            parent_thread_id: None,
            timestamp: "2025-01-03T12:00:00Z".to_string(),
            cwd: parent.to_path_buf(),
            runtime_workspace_roots: None,
            originator: "test".to_string(),
            cli_version: "test".to_string(),
            source: SessionSource::Cli,
            thread_source: None,
            agent_path: None,
            agent_nickname: None,
            agent_role: None,
            model_provider: None,
            base_instructions: None,
            dynamic_tools: None,
            selected_capability_roots: Vec::new(),
            memory_mode: None,
            history_mode: Default::default(),
            history_base: None,
            subagent_history_start_ordinal: None,
            multi_agent_version: None,
            context_window: None,
        },
        git: None,
    };
    let lines = [
        RolloutLine {
            timestamp: "2025-01-03T12:00:00Z".to_string(),
            ordinal: None,
            item: RolloutItem::SessionMeta(Box::new(session_meta_line)),
        },
        RolloutLine {
            timestamp: "2025-01-03T12:00:01Z".to_string(),
            ordinal: None,
            item: RolloutItem::EventMsg(EventMsg::UserMessage(UserMessageEvent {
                message: message.to_string(),
                ..Default::default()
            })),
        },
    ];
    let jsonl = lines
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()?
        .join("\n");
    fs::write(path, format!("{jsonl}\n"))?;
    Ok(())
}

fn compress_now(path: &std::path::Path) -> anyhow::Result<()> {
    let compressed_path = path::compressed_rollout_path(path);
    let input = fs::File::open(path)?;
    let output = fs::File::create(compressed_path)?;
    let mut encoder = zstd::stream::write::Encoder::new(output, 3)?;
    // Declares a window past the decoder's default 128 MiB limit, as legacy `--long=31` frames do.
    encoder.window_log(28)?;
    let mut input = std::io::BufReader::new(input);
    std::io::copy(&mut input, &mut encoder)?;
    encoder.finish()?;
    fs::remove_file(path)?;
    Ok(())
}
