//! Serves `debug/flightRecorder/read` from the process ring alone: no thread, config or
//! session state, so a wedged thread cannot block the read that diagnoses it.

use codex_app_server_protocol::FlightRecorderLevel;
use codex_app_server_protocol::FlightRecorderReadParams;
use codex_app_server_protocol::FlightRecorderReadResponse;
use codex_app_server_protocol::FlightRecorderRecord;
use codex_app_server_protocol::FlightRecorderTier;
use codex_app_server_protocol::FlightRecorderTierName;
use codex_app_server_protocol::JSONRPCErrorError;
use codex_flight_recorder::FlightRecorder;
use codex_flight_recorder::RecordFilter;
use codex_flight_recorder::Tier;
use tracing::Level;

use crate::error_code::invalid_request;

/// Records returned when the request names no limit.
const DEFAULT_LIMIT: usize = 2000;

pub(crate) fn read_flight_recorder(
    params: FlightRecorderReadParams,
) -> Result<FlightRecorderReadResponse, JSONRPCErrorError> {
    let Some(recorder) = FlightRecorder::global() else {
        return Err(invalid_request("this process has no flight recorder"));
    };
    let FlightRecorderReadParams {
        thread_id,
        min_level,
        target_prefix,
        since_unix_ms,
        until_unix_ms,
        after_seq,
        limit,
    } = params;
    let excerpt = recorder.excerpt(&RecordFilter {
        thread_id,
        min_level: min_level.map(level),
        target_prefix,
        since_unix_ms,
        until_unix_ms,
        after_seq,
        limit: Some(limit.map_or(DEFAULT_LIMIT, |limit| limit as usize)),
    });
    Ok(FlightRecorderReadResponse {
        records: excerpt
            .records
            .into_iter()
            .map(|record| FlightRecorderRecord {
                seq: record.seq,
                unix_ms: record.unix_ms,
                level: wire_level(record.level),
                target: record.target.to_owned(),
                thread_id: record.thread_id.map(String::from),
                text: record.text.into(),
            })
            .collect(),
        tiers: excerpt
            .tiers
            .into_iter()
            .map(|usage| FlightRecorderTier {
                tier: match usage.tier {
                    Tier::Alert => FlightRecorderTierName::Alert,
                    Tier::Info => FlightRecorderTierName::Info,
                    Tier::Chatter => FlightRecorderTierName::Chatter,
                },
                budget_bytes: usage.budget_bytes as u64,
                used_bytes: usage.used_bytes as u64,
                oldest_unix_ms: usage.oldest_unix_ms,
            })
            .collect(),
    })
}

fn level(level: FlightRecorderLevel) -> Level {
    match level {
        FlightRecorderLevel::Error => Level::ERROR,
        FlightRecorderLevel::Warn => Level::WARN,
        FlightRecorderLevel::Info => Level::INFO,
        FlightRecorderLevel::Debug => Level::DEBUG,
        FlightRecorderLevel::Trace => Level::TRACE,
    }
}

fn wire_level(level: Level) -> FlightRecorderLevel {
    match level {
        Level::ERROR => FlightRecorderLevel::Error,
        Level::WARN => FlightRecorderLevel::Warn,
        Level::INFO => FlightRecorderLevel::Info,
        Level::DEBUG => FlightRecorderLevel::Debug,
        Level::TRACE => FlightRecorderLevel::Trace,
    }
}
