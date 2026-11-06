use pretty_assertions::assert_eq;
use tracing::Level;
use tracing_subscriber::prelude::*;

use super::*;

fn texts(excerpt: &Excerpt) -> Vec<&str> {
    excerpt.records.iter().map(|record| &*record.text).collect()
}

/// A flood of chatter must not evict the WARN and INFO records that explain an incident.
#[test]
fn chatter_evicts_only_chatter() {
    let recorder = FlightRecorder::new(Budgets {
        alert: 1024,
        info: 1024,
        chatter: 1024,
    });
    recorder.push(1, Level::WARN, "t", None, "warned".into());
    recorder.push(2, Level::INFO, "t", None, "informed".into());
    for at in 3..1_000 {
        recorder.push(
            at,
            Level::TRACE,
            "zbus",
            None,
            format!("chatter {at}").into(),
        );
    }
    let excerpt = recorder.excerpt(&RecordFilter::default());
    let texts = texts(&excerpt);
    assert_eq!(texts[..2], ["warned", "informed"]);
    assert_eq!(texts.last(), Some(&"chatter 999"));
    assert!(excerpt.tiers[2].used_bytes <= 1024);
    assert!(excerpt.tiers[2].oldest_unix_ms > Some(3));
}

/// Async work often logs outside its thread's span, so an event's own field must win.
#[test]
fn records_carry_the_innermost_thread_attribution() {
    let recorder = FlightRecorder::new(Budgets::CLIENT);
    let subscriber = tracing_subscriber::registry().with(recorder.layer());
    tracing::subscriber::with_default(subscriber, || {
        tracing::info_span!("turn", thread_id = %"span-thread").in_scope(|| {
            tracing::warn!("inside the span");
            tracing::info!(thread_id = "field-thread", "explicitly attributed");
        });
        tracing::info!("unattributed");
    });
    let by_thread = |thread_id: &str| {
        recorder.excerpt(&RecordFilter {
            thread_id: Some(thread_id.to_string()),
            ..Default::default()
        })
    };
    let span_records = by_thread("span-thread");
    assert_eq!(
        texts(&span_records),
        ["turn{thread_id=span-thread}: inside the span"]
    );
    let field_records = by_thread("field-thread");
    assert_eq!(field_records.records.len(), 1);
    assert_eq!(field_records.records[0].level, Level::INFO);
    let warnings = recorder.excerpt(&RecordFilter {
        min_level: Some(Level::WARN),
        ..Default::default()
    });
    assert_eq!(warnings.records.len(), 1);
}
