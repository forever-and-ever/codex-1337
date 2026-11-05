use codex_otel::SessionTelemetry;
use codex_protocol::MemoryVersion;
use codex_protocol::config_types::ReasoningSummary;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;

use super::memory_metric_tags;

#[derive(Clone, Debug)]
pub(crate) struct StageOneRequestContext {
    pub(super) version: MemoryVersion,
    pub(crate) model_info: ModelInfo,
    pub(crate) session_telemetry: SessionTelemetry,
    pub(crate) reasoning_effort: Option<ReasoningEffort>,
    pub(crate) reasoning_summary: ReasoningSummary,
    pub(crate) service_tier: Option<String>,
}

impl StageOneRequestContext {
    pub(crate) fn start_timer(&self, name: &str) -> Option<codex_otel::Timer> {
        self.session_telemetry
            .start_timer(name, &memory_metric_tags(self.version, &[]))
            .ok()
    }

    pub(crate) fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]) {
        self.session_telemetry
            .counter(name, inc, &memory_metric_tags(self.version, tags));
    }

    pub(crate) fn histogram(&self, name: &str, value: i64, tags: &[(&str, &str)]) {
        self.session_telemetry
            .histogram(name, value, &memory_metric_tags(self.version, tags));
    }
}
