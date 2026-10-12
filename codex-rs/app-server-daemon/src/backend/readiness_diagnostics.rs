use std::path::Path;

use tokio::fs;

use super::PidRecord;
use super::stderr_log_file_for_pid_file;

pub(crate) async fn readiness_failure_context(
    pid_file: &Path,
    socket_path: &Path,
    codex_bin: &Path,
) -> String {
    // Failure reporting must not launch another copy of the stalled executable.
    let mut context = format!(
        "app server did not become ready on {}\n\nDaemon used app-server:\n  path: {}",
        socket_path.display(),
        codex_bin.display()
    );
    let pid_state = pid_state(pid_file).await;
    let stderr_path = stderr_log_file_for_pid_file(pid_file);
    let stderr_state = match fs::metadata(&stderr_path).await {
        Ok(metadata) if metadata.len() == 0 => "empty".to_string(),
        Ok(metadata) => format!("nonempty ({} bytes)", metadata.len()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => "missing".to_string(),
        Err(err) => format!("unavailable: {err}"),
    };
    let socket_state = match fs::symlink_metadata(socket_path).await {
        Ok(metadata) => format!("present ({:?})", metadata.file_type()),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => "missing".to_string(),
        Err(err) => format!("unavailable: {err}"),
    };
    context.push_str(&format!(
        "\n\nReadiness failure snapshot:\n  PID record ({}): {pid_state}\n  stderr ({}): {stderr_state}\n  socket ({}): {socket_state}",
        pid_file.display(),
        stderr_path.display(),
        socket_path.display(),
    ));
    super::super::append_stderr_log_tail_context(pid_file, &mut context).await;
    context
}

async fn pid_state(path: &Path) -> String {
    let contents = match fs::read(path).await {
        Ok(contents) => contents,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return "missing".to_string(),
        Err(err) => return format!("unavailable: {err}"),
    };
    if contents.iter().all(u8::is_ascii_whitespace) {
        return "empty reservation".to_string();
    }
    let record = match serde_json::from_slice::<PidRecord>(&contents) {
        Ok(record) => record,
        Err(err) => return format!("invalid: {err}"),
    };
    format!("pid={}: {}", record.pid, process_state(&record).await)
}

async fn process_state(record: &PidRecord) -> String {
    #[cfg(unix)]
    if !super::process_exists(record.pid) {
        return "process absent".to_string();
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if let Some(expected) = &record.process_identity {
        // Unlike process_matches_record, this observation never reaps a zombie.
        return match expected.matches_process(record.pid).await {
            Ok((zombie, matches)) => format!("identity_matches={matches}, zombie={zombie}"),
            Err(err) => format!("identity unavailable: {err:#}"),
        };
    }
    format!(
        "native identity unavailable; recorded start time: {}",
        record.process_start_time
    )
}
