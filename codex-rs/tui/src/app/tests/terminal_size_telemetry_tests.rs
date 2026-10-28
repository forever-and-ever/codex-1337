//! Terminal-size observation eligibility follows the config reloaded for resume.

use super::*;
use pretty_assertions::assert_eq;

#[tokio::test]
async fn terminal_size_telemetry_respects_reloaded_exporter_and_consent() -> Result<()> {
    let (mut app, _app_event_rx, _op_rx) = Box::pin(make_test_app_with_channels()).await;
    let project = tempdir()?;
    let cwd = project.path().to_path_buf();
    let current_config = app.config.clone();
    let current_settings = app.local_settings.clone();
    for config in [
        "[otel]\nmetrics_exporter = \"statsig\"\n",
        "[otel]\nmetrics_exporter = \"statsig\"\n[analytics]\nenabled = false\n",
        "analytics.enabled = false\n[otel.metrics_exporter.otlp-http]\nendpoint = \"http://127.0.0.1:4318/v1/metrics\"\nprotocol = \"json\"\n",
    ] {
        std::fs::write(app.config.codex_home.join("config.toml"), config)?;
        let error = app
            .capture_config_rebuild(cwd.clone(), /*current_cwd*/ None)
            .run()
            .await
            .err()
            .expect("removed exporters must be rejected on reload");
        assert!(
            format!("{error:#}").contains(
                "OpenTelemetry exporters are not supported by this distribution; set all otel exporters to none"
            ),
            "{error:#}"
        );
        assert_eq!(app.config, current_config);
        assert_eq!(app.local_settings, current_settings);
    }
    for consent in [true, false] {
        std::fs::write(
            app.config.codex_home.join("config.toml"),
            format!("[otel]\nmetrics_exporter = \"none\"\n[analytics]\nenabled = {consent}\n"),
        )?;
        let (config, local_settings) = *app
            .capture_config_rebuild(cwd.clone(), /*current_cwd*/ None)
            .run()
            .await?;
        app.config = config;
        app.local_settings = local_settings;
        assert!(!crate::terminal_size_telemetry::enabled(&app.config));
        assert_eq!(app.config.analytics_enabled, Some(consent));
    }
    Ok(())
}
