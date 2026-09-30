use super::Result;
use std::time::Duration;

/// Export instrumentation is disabled; never buffer data without a consumer.
pub fn record_global_operation(
    _count_name: &str,
    _duration_name: &str,
    _duration: Duration,
    _tags: &[(&str, &str)],
) -> Result<()> {
    Ok(())
}
