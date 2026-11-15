//! `codex logs`: reads the shared server's in-memory flight recorder.
//!
//! The read is unserialized and touches no thread state, so it answers while a thread is wedged.
//! If the server cannot answer at all, `systemctl --user kill --kill-whom=main -s SIGUSR1
//! codex-app-server.service` dumps the ring under `$CODEX_HOME/log/flight-recorder/`.

use std::time::Duration;

use anyhow::Context;
use chrono::DateTime;
use chrono::SecondsFormat;
use chrono::Utc;
use clap::Args;
use clap::ValueEnum;
use codex_app_server_client::RemoteAppServerClient;
use codex_app_server_client::RemoteAppServerConnectArgs;
use codex_app_server_client::RemoteAppServerEndpoint;
use codex_app_server_client::app_server_control_socket_path;
use codex_app_server_protocol::ClientRequest;
use codex_app_server_protocol::FlightRecorderLevel;
use codex_app_server_protocol::FlightRecorderReadParams;
use codex_app_server_protocol::FlightRecorderReadResponse;
use codex_app_server_protocol::FlightRecorderRecord;
use codex_app_server_protocol::FlightRecorderTier;
use codex_app_server_protocol::RequestId;
use codex_core::config::find_codex_home;

const READ_TIMEOUT: Duration = Duration::from_secs(10);
const FOLLOW_INTERVAL: Duration = Duration::from_secs(1);
const UNANSWERED: &str = "the shared server did not answer; send it SIGUSR1 to dump its recorder";

#[derive(Debug, Args)]
pub struct LogsCommand {
    /// Only records attributed to this thread.
    #[arg(long)]
    thread: Option<String>,
    /// Least severe level shown.
    #[arg(long, value_enum)]
    level: Option<LogLevel>,
    /// Only targets starting with this prefix, e.g. `codex_core::session`.
    #[arg(long)]
    target: Option<String>,
    /// Records at or after this instant: a span ago (`90s`, `10m`, `2h`, `1d`) or RFC 3339.
    #[arg(long, value_parser = parse_instant)]
    since: Option<i64>,
    /// Records at or before this instant, in the same forms as `--since`.
    #[arg(long, value_parser = parse_instant)]
    until: Option<i64>,
    /// Newest matching records to print; the server's default is 2000.
    #[arg(long)]
    limit: Option<u32>,
    /// Print the response as JSON.
    #[arg(long)]
    json: bool,
    /// Keep printing records as they arrive.
    #[arg(long, conflicts_with_all = ["until", "json"])]
    follow: bool,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum LogLevel {
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl From<LogLevel> for FlightRecorderLevel {
    fn from(level: LogLevel) -> Self {
        match level {
            LogLevel::Error => Self::Error,
            LogLevel::Warn => Self::Warn,
            LogLevel::Info => Self::Info,
            LogLevel::Debug => Self::Debug,
            LogLevel::Trace => Self::Trace,
        }
    }
}

/// Unix milliseconds from a span before now or an RFC 3339 instant.
fn parse_instant(value: &str) -> Result<i64, String> {
    if let Ok(instant) = DateTime::parse_from_rfc3339(value) {
        return Ok(instant.timestamp_millis());
    }
    let split = value
        .find(|character: char| !character.is_ascii_digit())
        .ok_or_else(|| format!("{value:?} needs a unit: s, m, h or d"))?;
    let (count, unit) = value.split_at(split);
    let count: i64 = count
        .parse()
        .map_err(|_| format!("{value:?} is neither a span nor RFC 3339"))?;
    let unit_ms = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        _ => return Err(format!("{value:?} needs a unit: s, m, h or d")),
    };
    Ok(Utc::now().timestamp_millis() - count * unit_ms)
}

fn rfc3339(unix_ms: i64) -> String {
    DateTime::from_timestamp_millis(unix_ms).map_or_else(
        || unix_ms.to_string(),
        |at| at.to_rfc3339_opts(SecondsFormat::Millis, true),
    )
}

pub async fn read_flight_recorder(args: LogsCommand) -> anyhow::Result<()> {
    let socket_path = app_server_control_socket_path(find_codex_home()?.as_path())?;
    let client = tokio::time::timeout(
        READ_TIMEOUT,
        RemoteAppServerClient::connect(RemoteAppServerConnectArgs {
            required_human_approval: false,
            endpoint: RemoteAppServerEndpoint::UnixSocket { socket_path },
            client_name: "codex-logs".into(),
            client_version: env!("CARGO_PKG_VERSION").into(),
            experimental_api: true,
            mcp_server_openai_form_elicitation: false,
            opt_out_notification_methods: Vec::new(),
            channel_capacity: 16,
        }),
    )
    .await
    .context(UNANSWERED)??;
    let result = print_excerpts(&client, args).await;
    let _ = tokio::time::timeout(Duration::from_millis(/*millis*/ 200), client.shutdown()).await;
    result
}

async fn print_excerpts(client: &RemoteAppServerClient, args: LogsCommand) -> anyhow::Result<()> {
    let mut params = FlightRecorderReadParams {
        thread_id: args.thread,
        min_level: args.level.map(Into::into),
        target_prefix: args.target,
        since_unix_ms: args.since,
        until_unix_ms: args.until,
        after_seq: None,
        limit: args.limit,
    };
    for request_id in 1.. {
        let response: FlightRecorderReadResponse = tokio::time::timeout(
            READ_TIMEOUT,
            client.request_typed(ClientRequest::FlightRecorderRead {
                request_id: RequestId::Integer(request_id),
                params: params.clone(),
            }),
        )
        .await
        .context(UNANSWERED)??;
        if args.json {
            println!("{}", serde_json::to_string(&response)?);
            return Ok(());
        }
        for record in &response.records {
            print_record(record)?;
        }
        if !args.follow {
            return print_tiers(&response.tiers);
        }
        if let Some(record) = response.records.last() {
            params.after_seq = Some(record.seq);
        }
        tokio::time::sleep(FOLLOW_INTERVAL).await;
    }
    Ok(())
}

fn print_record(record: &FlightRecorderRecord) -> anyhow::Result<()> {
    let level = serde_json::to_value(record.level)?;
    let thread = record
        .thread_id
        .as_deref()
        .map(|thread_id| format!(" thread_id={thread_id}"))
        .unwrap_or_default();
    println!(
        "{} {:>5} {}{thread}: {}",
        rfc3339(record.unix_ms),
        level.as_str().unwrap_or_default().to_uppercase(),
        record.target,
        record.text
    );
    Ok(())
}

fn print_tiers(tiers: &[FlightRecorderTier]) -> anyhow::Result<()> {
    for tier in tiers {
        let name = serde_json::to_value(tier.tier)?;
        eprintln!(
            "{:<7} {:>6}/{} MiB, since {}",
            name.as_str().unwrap_or_default(),
            mebibytes(tier.used_bytes),
            mebibytes(tier.budget_bytes),
            tier.oldest_unix_ms.map_or_else(|| "-".to_owned(), rfc3339),
        );
    }
    Ok(())
}

/// Two decimals, truncated.
fn mebibytes(bytes: u64) -> String {
    format!("{}.{:02}", bytes >> 20, ((bytes & 0xF_FFFF) * 100) >> 20)
}
