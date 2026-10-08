use super::AnthropicPromptCache;
use codex_protocol::openai_models::AnthropicCacheTtl;
use codex_protocol::openai_models::ReasoningEffort;
use pretty_assertions::assert_eq;

#[test]
fn ordered_rules_filter_effective_effort_and_validate_selectors() {
    let policy: AnthropicPromptCache = toml::from_str(
        r#"
        [[rules]]
        model = "claude-sonnet-5"
        efforts = ["high"]
        ttl = "1h"
        [[rules]]
        model = "claude-sonnet"
        efforts = ["low", "high"]
        ttl = "5m"
        [[rules]]
        model = "claude-haiku"
        ttl = "5m"
        "#,
    )
    .unwrap();
    for (model, effort, expected) in [
        (
            "claude-sonnet-5-5",
            Some(ReasoningEffort::High),
            AnthropicCacheTtl::OneHour,
        ),
        (
            "claude-sonnet-5-5",
            Some(ReasoningEffort::Low),
            AnthropicCacheTtl::FiveMinutes,
        ),
        (
            "claude-sonnet-5-5",
            Some(ReasoningEffort::Medium),
            AnthropicCacheTtl::OneHour,
        ),
        ("claude-sonnet-5-5", None, AnthropicCacheTtl::OneHour),
        ("claude-haiku-5", None, AnthropicCacheTtl::FiveMinutes),
        (
            "Claude-sonnet-5-5",
            Some(ReasoningEffort::Low),
            AnthropicCacheTtl::OneHour,
        ),
    ] {
        assert_eq!(policy.resolve(model, effort.as_ref()), expected, "{model}");
    }
    for policy in [AnthropicPromptCache::default(), toml::from_str("").unwrap()] {
        assert_eq!(
            policy.resolve("claude-opus-5-5", None),
            AnthropicCacheTtl::OneHour
        );
    }
    let overridden: AnthropicPromptCache = toml::from_str("ttl = '5m'").unwrap();
    assert_eq!(
        overridden.resolve("claude-opus-5-5", None),
        AnthropicCacheTtl::FiveMinutes
    );
    for invalid in [
        "[[rules]]\nmodel = 'claude-sonnet'\nefforts = []\nttl = '5m'",
        "[[rules]]\nmodel = ''\nttl = '5m'",
    ] {
        assert!(toml::from_str::<AnthropicPromptCache>(invalid).is_err());
    }
}
