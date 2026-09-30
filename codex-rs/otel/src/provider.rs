use crate::config::OtelExporter;
use crate::config::OtelSettings;
use crate::metrics::MetricsClient;
use std::error::Error;
use std::io;
use std::time::Duration;
use tracing_subscriber::layer::Identity;

/// Export providers are unavailable in this distribution.
pub enum OtelProvider {}

impl OtelProvider {
    pub fn try_new(settings: &OtelSettings) -> Result<Option<Self>, Box<dyn Error>> {
        if !matches!(settings.exporter, OtelExporter::None)
            || !matches!(settings.trace_exporter, OtelExporter::None)
            || !matches!(settings.metrics_exporter, OtelExporter::None)
        {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "OpenTelemetry exporters are not supported by this distribution",
            )
            .into());
        }
        if settings.runtime_metrics {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "features.runtime_metrics is not supported by this distribution",
            )
            .into());
        }
        Ok(None)
    }

    pub fn metrics(&self) -> Option<&MetricsClient> {
        match *self {}
    }

    pub fn logger_layer(&self) -> Option<Identity> {
        match *self {}
    }

    pub fn logger_export_layer(&self) -> Option<Identity> {
        match *self {}
    }

    pub fn tracing_layer(&self) -> Option<Identity> {
        match *self {}
    }

    pub fn log_export_filter(_meta: &tracing::Metadata<'_>) -> bool {
        false
    }

    pub fn trace_export_filter(_meta: &tracing::Metadata<'_>) -> bool {
        false
    }

    pub fn codex_export_filter(_meta: &tracing::Metadata<'_>) -> bool {
        false
    }

    pub fn shutdown(&self) {
        match *self {}
    }

    pub async fn shutdown_with_timeout(self, _timeout: Duration) -> io::Result<()> {
        match self {}
    }
}
