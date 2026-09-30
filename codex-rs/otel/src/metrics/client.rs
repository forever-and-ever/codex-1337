use super::MetricsError;
use super::Result;
use super::timer::Timer;
use std::time::Duration;

/// No metrics client can be installed without an exporter.
#[derive(Clone, Debug)]
pub enum MetricsClient {}

impl MetricsClient {
    pub fn counter(&self, name: &str, inc: i64, tags: &[(&str, &str)]) -> Result<()> {
        let _ = (name, inc, tags);
        match *self {}
    }

    pub fn counter_with_description(
        &self,
        name: &str,
        description: &str,
        inc: i64,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let _ = (name, description, inc, tags);
        match *self {}
    }

    pub fn histogram(&self, name: &str, value: i64, tags: &[(&str, &str)]) -> Result<()> {
        let _ = (name, value, tags);
        match *self {}
    }

    pub fn histogram_with_boundaries(
        &self,
        name: &str,
        value: i64,
        boundaries: &[f64],
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let _ = (name, value, boundaries, tags);
        match *self {}
    }

    pub fn gauge(&self, name: &str, value: i64, tags: &[(&str, &str)]) -> Result<()> {
        let _ = (name, value, tags);
        match *self {}
    }

    pub fn gauge_with_description(
        &self,
        name: &str,
        description: &str,
        value: i64,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let _ = (name, description, value, tags);
        match *self {}
    }

    pub fn register_observable_gauge_with_description(
        &self,
        name: &str,
        description: &str,
        observe: impl Fn() -> i64 + Send + Sync + 'static,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let _ = (name, description, observe, tags);
        match *self {}
    }

    pub fn record_duration(
        &self,
        name: &str,
        duration: Duration,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let _ = (name, duration, tags);
        match *self {}
    }

    pub(crate) fn record_duration_ms_f64(
        &self,
        name: &str,
        duration_ms: f64,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let _ = (name, duration_ms, tags);
        match *self {}
    }

    pub fn record_duration_seconds_with_description(
        &self,
        name: &str,
        description: &str,
        duration: Duration,
        tags: &[(&str, &str)],
    ) -> Result<()> {
        let _ = (name, description, duration, tags);
        match *self {}
    }

    pub fn start_timer(
        &self,
        name: &str,
        tags: &[(&str, &str)],
    ) -> std::result::Result<Timer, MetricsError> {
        let _ = (name, tags);
        match *self {}
    }

    pub fn shutdown(&self) -> Result<()> {
        match *self {}
    }
}
