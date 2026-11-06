//! Formats tracing events into recorder records, attributing each to its thread.

use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use tracing::Event;
use tracing::Subscriber;
use tracing::field::Field;
use tracing::field::Visit;
use tracing::span::Attributes;
use tracing::span::Id;
use tracing::span::Record as SpanValues;
use tracing_subscriber::Layer;
use tracing_subscriber::filter::LevelFilter;
use tracing_subscriber::filter::Targets;
use tracing_subscriber::fmt::FormatFields;
use tracing_subscriber::fmt::FormattedFields;
use tracing_subscriber::fmt::format::DefaultFields;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

use crate::FlightRecorder;

/// Longest text kept per record; full diffs and payload dumps keep only their head.
const MAX_RECORD_TEXT_BYTES: usize = 64 << 10;
const TRUNCATION_MARK: &str = " …[truncated]";

pub(crate) fn filtered<S>(recorder: FlightRecorder) -> impl Layer<S> + Send + Sync + 'static
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    RecorderLayer { recorder }.with_filter(targets())
}

fn targets() -> Targets {
    Targets::new()
        .with_default(LevelFilter::TRACE)
        // Opted-in content belongs to the configured OTLP destination, never the recorder.
        .with_target("codex_otel.log_only", LevelFilter::OFF)
        .with_target("codex_otel.trace_safe", LevelFilter::OFF)
        // TRACE on these carries full request bodies, streamed responses, websocket frames and
        // authenticated handshakes.
        .with_target("codex_http_client::transport", LevelFilter::DEBUG)
        .with_target("codex_api::sse", LevelFilter::DEBUG)
        .with_target("tungstenite", LevelFilter::DEBUG)
        .with_target("codex_api::responses_websocket_timing", LevelFilter::OFF)
        .with_target("codex_core::post_sampling_token_estimate", LevelFilter::OFF)
        // Per-frame transport and per-statement database chatter.
        .with_target("h2", LevelFilter::WARN)
        .with_target("hyper_util", LevelFilter::WARN)
        .with_target("sqlx", LevelFilter::WARN)
        // D-Bus marshalling under each keyring lookup was two thirds of all DEBUG and TRACE bytes.
        .with_target("zbus", LevelFilter::INFO)
        .with_target("keyring", LevelFilter::DEBUG)
        .with_target("tokio::", LevelFilter::INFO)
        .with_target("runtime::", LevelFilter::INFO)
        .with_target("codex_tui::streaming::controller", LevelFilter::DEBUG)
        .with_target("codex_tui::streaming::table_holdback", LevelFilter::DEBUG)
}

pub(crate) struct RecorderLayer {
    recorder: FlightRecorder,
}

/// Span name, formatted fields and nearest `thread_id`, cached when the span opens.
struct SpanContext {
    name: &'static str,
    fields: String,
    thread_id: Option<String>,
}

#[derive(Default)]
struct ThreadIdVisitor(Option<String>);

impl Visit for ThreadIdVisitor {
    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "thread_id" && self.0.is_none() {
            self.0 = Some(value.to_owned());
        }
    }

    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        if field.name() == "thread_id" && self.0.is_none() {
            self.0 = Some(format!("{value:?}"));
        }
    }
}

impl<S> Layer<S> for RecorderLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_new_span(&self, attrs: &Attributes<'_>, id: &Id, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut thread_id = ThreadIdVisitor::default();
        attrs.record(&mut thread_id);
        let mut fields = FormattedFields::<DefaultFields>::new(String::new());
        let _ = DefaultFields::default().format_fields(fields.as_writer(), attrs);
        span.extensions_mut().insert(SpanContext {
            name: span.metadata().name(),
            fields: fields.fields,
            thread_id: thread_id.0,
        });
    }

    fn on_record(&self, id: &Id, values: &SpanValues<'_>, ctx: Context<'_, S>) {
        let Some(span) = ctx.span(id) else {
            return;
        };
        let mut extensions = span.extensions_mut();
        let Some(context) = extensions.get_mut::<SpanContext>() else {
            return;
        };
        let mut thread_id = ThreadIdVisitor::default();
        values.record(&mut thread_id);
        if thread_id.0.is_some() {
            context.thread_id = thread_id.0;
        }
        let mut fields = FormattedFields::<DefaultFields>::new(std::mem::take(&mut context.fields));
        let _ = DefaultFields::default().add_fields(&mut fields, values);
        context.fields = fields.fields;
    }

    fn on_event(&self, event: &Event<'_>, ctx: Context<'_, S>) {
        let metadata = event.metadata();
        // An event's own `thread_id` field wins: async work often runs outside its thread's span.
        let mut thread_id = ThreadIdVisitor::default();
        event.record(&mut thread_id);
        let mut text = String::new();
        let mut span_thread_id = None;
        if let Some(scope) = ctx.event_scope(event) {
            for span in scope.from_root() {
                let extensions = span.extensions();
                match extensions.get::<SpanContext>() {
                    Some(context) => {
                        text.push_str(context.name);
                        if !context.fields.is_empty() {
                            text.push('{');
                            text.push_str(&context.fields);
                            text.push('}');
                        }
                        if context.thread_id.is_some() {
                            span_thread_id.clone_from(&context.thread_id);
                        }
                    }
                    None => text.push_str(span.metadata().name()),
                }
                text.push(':');
            }
            if !text.is_empty() {
                text.push(' ');
            }
        }
        let mut fields = FormattedFields::<DefaultFields>::new(text);
        let _ = DefaultFields::default().format_fields(fields.as_writer(), event);
        let mut text = fields.fields;
        if text.len() > MAX_RECORD_TEXT_BYTES {
            let mut cut = MAX_RECORD_TEXT_BYTES;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            text.truncate(cut);
            text.push_str(TRUNCATION_MARK);
        }
        self.recorder.push(
            unix_ms(SystemTime::now()),
            *metadata.level(),
            metadata.target(),
            thread_id.0.or(span_thread_id).map(String::into_boxed_str),
            text.into_boxed_str(),
        );
    }
}

pub(crate) fn unix_ms(at: SystemTime) -> i64 {
    at.duration_since(UNIX_EPOCH).map_or(0, |elapsed| {
        i64::try_from(elapsed.as_millis()).unwrap_or(i64::MAX)
    })
}
