use super::claude_quota_data;
use crate::clock_format::ClockFormat;
use crate::history_cell::HistoryCell;
use crate::legacy_core::config::ConfigBuilder;
use crate::status::StatusAccountDisplay;
use crate::status::new_status_output_with_rate_limits_handle;
use crate::test_support::PathBufExt;
use crate::test_support::test_path_buf;
use crate::token_usage::TokenUsage;
use chrono::Duration;
use chrono::Local;
use chrono::TimeZone;
use codex_app_server_protocol::AccountQuota;
use codex_app_server_protocol::AccountQuotaWindow;
use codex_config::LoaderOverrides;
use tempfile::TempDir;

#[tokio::test]
async fn status_snapshot_claude_subscription_quota() {
    let temp_home = TempDir::new().expect("temp home");
    let mut config = ConfigBuilder::default()
        .codex_home(temp_home.path().to_path_buf())
        .loader_overrides(LoaderOverrides::without_managed_config_for_tests())
        .build()
        .await
        .expect("load config");
    config.model_provider_id = "claude_subscription".to_owned();
    config.model_provider = config.model_providers[&config.model_provider_id].clone();
    config.cwd = test_path_buf("/workspace/tests").abs();
    config.workspace_roots = vec![config.cwd.clone()];
    config
        .permissions
        .set_workspace_roots(config.workspace_roots.clone());
    let now = Local
        .with_ymd_and_hms(2024, 1, 2, 3, 4, 5)
        .single()
        .expect("timestamp");
    let account = StatusAccountDisplay::ClaudeSubscription {
        email: Some("claude@example.com".to_owned()),
    };
    let handle = new_status_output_with_rate_limits_handle(
        &config,
        /*requires_openai_auth*/ false,
        Some(&config.model_provider_id),
        /*remote_connection*/ None,
        Some(&account),
        /*token_info*/ None,
        &TokenUsage::default(),
        &None,
        /*thread_name*/ None,
        /*forked_from*/ None,
        &[],
        /*_plan_type*/ None,
        now,
        "claude-sonnet-5-5",
        /*collaboration_mode*/ None,
        /*reasoning_effort_override*/ None,
        "<none>".to_owned(),
        /*refreshing_rate_limits*/ true,
    );
    let cell = handle.cell();
    let quota = AccountQuota {
        id: "claude".to_owned(),
        email: Some("claude@example.com".to_owned()),
        nickname: None,
        five_hour: Some(AccountQuotaWindow {
            remaining_percent: 75.0,
            resets_at: Some((now + Duration::hours(2)).timestamp()),
        }),
        seven_day: Some(AccountQuotaWindow {
            remaining_percent: 40.0,
            resets_at: Some((now + Duration::days(4)).timestamp()),
        }),
        captured_at: Some(now.timestamp()),
        retry_after_sec: 300,
        error: None,
    };
    assert!(!handle.is_ready());
    handle.finish_claude_quota_refresh(claude_quota_data(
        Some(&quota),
        now,
        ClockFormat::TwentyFourHour,
    ));
    assert!(handle.is_ready());
    let rendered = cell
        .display_lines(/*width*/ 80)
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!("status_snapshot_claude_subscription_quota", rendered);
}
