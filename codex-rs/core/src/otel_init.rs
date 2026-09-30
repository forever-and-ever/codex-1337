use crate::config::Config;
use codex_config::types::OtelExporterKind;
use codex_features::Feature;
use codex_otel::OtelProvider;
use std::error::Error;

/// Validates telemetry configuration without installing exporters.
pub fn build_provider(
    config: &Config,
    service_version: &str,
    service_name_override: Option<&str>,
    default_analytics_enabled: bool,
) -> Result<Option<OtelProvider>, Box<dyn Error>> {
    let _ = (
        service_version,
        service_name_override,
        default_analytics_enabled,
    );
    if [
        &config.otel.exporter,
        &config.otel.trace_exporter,
        &config.otel.metrics_exporter,
    ]
    .into_iter()
    .any(|exporter| *exporter != OtelExporterKind::None)
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "OpenTelemetry exporters are not supported by this distribution",
        )
        .into());
    }
    if config.features.enabled(Feature::RuntimeMetrics) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "features.runtime_metrics is not supported by this distribution",
        )
        .into());
    }
    Ok(None)
}

pub fn record_process_start(otel: Option<&OtelProvider>, originator: &str) {
    let Some(metrics) = otel.and_then(OtelProvider::metrics) else {
        return;
    };
    let _ = codex_otel::record_process_start_once(metrics, originator);
}

pub fn install_sqlite_telemetry(otel: Option<&OtelProvider>, originator: &str) {
    let Some(metrics) = otel.and_then(OtelProvider::metrics) else {
        return;
    };
    let telemetry = codex_rollout::sqlite_telemetry_recorder(metrics.clone(), originator);
    let _ = codex_state::install_process_db_telemetry(telemetry);
}
