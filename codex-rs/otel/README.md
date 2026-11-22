# codex-otel

This distribution keeps trace-context propagation and the existing event
instrumentation. The OpenTelemetry SDK, log/trace/metric exporters and runtime
metrics are not shipped. An exporter selected in configuration is rejected;
the compatibility façade never creates a provider, starts a header helper,
buffers export measurements or connects to a collector.

Local diagnostics use the bounded flight recorder, accessed with `codex logs`.
OpenTelemetry context propagation and local opt-in rollout trace bundles are
independent of the removed exporters.
