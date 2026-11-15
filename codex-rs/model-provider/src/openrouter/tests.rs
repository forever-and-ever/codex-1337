use super::super::Endpoint;
use super::super::Endpoints;
use super::super::Envelope;
use super::super::Model;
use super::super::Preset;
use super::*;
use codex_protocol::EndpointSet;
use std::collections::BTreeSet;

#[test]
fn normalization_contract() {
    let records = [
        (
            include_str!("fixtures/kimi-model.json"),
            include_str!("fixtures/kimi-endpoints.json"),
            "Kimi K3",
            "$0.29 in · $8.25 out per M · 1M context",
            ReasoningEffort::Max,
            false,
            vec!["priority"],
        ),
        (
            include_str!("fixtures/deepseek-model.json"),
            include_str!("fixtures/deepseek-endpoints.json"),
            "DeepSeek V4.1 Flash",
            "$0.04 in · $0.18 out per M · 1M context",
            ReasoningEffort::High,
            false,
            vec!["priority", "flex"],
        ),
        (
            include_str!("fixtures/openai-model.json"),
            include_str!("fixtures/openai-endpoints.json"),
            "GPT-5.5",
            "$2.50 in · $15.00 out per M · 1.05M context",
            ReasoningEffort::Medium,
            true,
            vec!["priority", "flex"],
        ),
    ];
    for (model_json, endpoints_json, name, description, default_effort, openai, tiers) in records {
        let model = serde_json::from_str::<Envelope<Model>>(model_json)
            .expect("recorded model")
            .data;
        let endpoints = serde_json::from_str::<Envelope<Endpoints>>(endpoints_json)
            .expect("recorded endpoints")
            .data
            .endpoints;
        let resolved = ResolvedModel {
            slug: model.id.clone(),
            model,
            endpoints,
        };
        assert!(resolved.model.supports_tools());
        let info = normalize(&resolved, &EndpointFilter::All, 7);
        assert_eq!(info.slug, resolved.slug.as_str());
        assert_eq!(info.display_name, name);
        assert_eq!(info.description.as_deref(), Some(description));
        assert_eq!(info.context_window, Some(resolved.model.context_length));
        assert_eq!(info.max_context_window, info.context_window);
        assert_eq!(info.default_reasoning_level, Some(default_effort));
        assert_eq!(info.supports_reasoning_summary_parameter, openai);
        assert_eq!(info.support_verbosity, openai);
        assert_eq!(info.visibility, ModelVisibility::List);
        assert_eq!(info.priority, 7);
        assert_eq!(
            info.input_modalities,
            vec![InputModality::Text, InputModality::Image]
        );
        assert_eq!(
            info.service_tiers
                .iter()
                .map(|tier| tier.id.as_str())
                .collect::<Vec<_>>(),
            tiers
        );
        assert_eq!(
            info.service_tiers
                .iter()
                .map(|tier| tier.name.as_str())
                .collect::<Vec<_>>(),
            if openai || name == "DeepSeek V4.1 Flash" {
                vec!["Fast", "Flex"]
            } else {
                vec!["Fast"]
            }
        );
        assert!(
            info.supported_reasoning_levels
                .windows(2)
                .all(|pair| effort_rank(&pair[0].effort) < effort_rank(&pair[1].effort))
        );
        assert!(
            info.supported_reasoning_levels
                .iter()
                .all(|preset| !preset.description.is_empty())
        );
        assert!(
            !info.supports_native_compaction
                && !info.supports_search_tool
                && !info.supports_experimental_context
                && !info.use_responses_lite
                && !info.supports_reasoning_effort_updates
        );
        assert!(
            info.available_access_programs.is_none()
                && info.upgrade.is_none()
                && info.availability_nux.is_none()
                && info.anthropic.is_none()
                && info.experimental_supported_tools.is_empty()
        );

        if openai {
            let bundled = bundled_models_response()
                .expect("bundled models")
                .models
                .into_iter()
                .find(|model| model.slug == "gpt-5.5")
                .expect("GPT shape");
            assert_eq!(info.model_messages, bundled.model_messages);
            assert_eq!(info.tool_mode, bundled.tool_mode);
            assert_eq!(info.apply_patch_tool_type, bundled.apply_patch_tool_type);
            assert_eq!(info.shell_type, bundled.shell_type);
            assert_eq!(info.truncation_policy, bundled.truncation_policy);
            assert_eq!(
                info.include_skills_usage_instructions,
                bundled.include_skills_usage_instructions
            );
            assert_eq!(
                info.include_plugin_usage_instructions,
                bundled.include_plugin_usage_instructions
            );
            assert_eq!(
                info.include_apps_usage_instructions,
                bundled.include_apps_usage_instructions
            );
        } else {
            assert_eq!(info.tool_mode, None);
            assert_eq!(info.apply_patch_tool_type, None);
        }
    }
    let model =
        serde_json::from_str::<Envelope<Model>>(include_str!("fixtures/deepseek-model.json"))
            .expect("deepseek model")
            .data;
    let endpoints = serde_json::from_str::<Envelope<Endpoints>>(include_str!(
        "fixtures/deepseek-endpoints.json"
    ))
    .expect("deepseek endpoints")
    .data
    .endpoints;
    let preset = serde_json::from_str::<Envelope<Preset>>(include_str!("fixtures/preset.json"))
        .expect("recorded preset")
        .data;
    assert_eq!(preset.designated_version.config.model, model.id);
    let mut resolved = ResolvedModel {
        slug: "@preset/codex-deepseek-v4-1-flash"
            .parse()
            .expect("preset slug"),
        model,
        endpoints,
    };
    let info = normalize(&resolved, &EndpointFilter::All, 0);
    assert_eq!(info.display_name, "codex-deepseek-v4-1-flash");
    assert!(
        info.description
            .expect("preset description")
            .ends_with(" · preset for deepseek/deepseek-v4.1-flash")
    );
    resolved.slug = resolved.model.id.clone();
    let only = EndpointFilter::Only(
        EndpointSet::new(BTreeSet::from(["inference-net".parse().expect("host")]))
            .expect("nonempty"),
    );
    let routed = normalize(&resolved, &only, 0);
    assert_eq!(routed.context_window, Some(1_040_000));
    assert_eq!(routed.service_tiers, []);
    assert_eq!(
        routed.description.as_deref(),
        Some("$0.10 in · $0.72 out per M · 1.04M context")
    );
    let deepseek = EndpointFilter::Only(
        EndpointSet::new(BTreeSet::from(["deepseek".parse().expect("host")])).expect("nonempty"),
    );
    assert_eq!(
        normalize(&resolved, &deepseek, 0).description.as_deref(),
        Some("$0.15 in · $0.60 out per M · 1M context")
    );
    let ignore = EndpointFilter::Ignore(
        EndpointSet::new(BTreeSet::from([
            "deepinfra".parse().expect("host"),
            "baseten".parse().expect("host"),
        ]))
        .expect("nonempty"),
    );
    assert_eq!(normalize(&resolved, &ignore, 0).service_tiers, []);
    resolved.endpoints.clear();
    assert_eq!(
        normalize(&resolved, &EndpointFilter::All, 0).context_window,
        Some(resolved.model.context_length)
    );
    resolved
        .model
        .reasoning
        .as_mut()
        .expect("reasoning")
        .mandatory = true;
    resolved.model.expiration_date = Some("2027-01-01".into());
    resolved.model.supported_parameters.clear();
    let mut endpoint: Endpoint = serde_json::from_str::<Envelope<Endpoints>>(include_str!(
        "fixtures/deepseek-endpoints.json"
    ))
    .expect("endpoint record")
    .data
    .endpoints
    .remove(0);
    endpoint.service_tier = Some("ultrafast".into());
    resolved.endpoints.push(endpoint);
    let constrained = normalize(&resolved, &EndpointFilter::All, 0);
    assert!(
        !constrained
            .supported_reasoning_levels
            .iter()
            .any(|preset| preset.effort == ReasoningEffort::None)
    );
    assert!(
        constrained
            .description
            .expect("retirement")
            .ends_with(" · retires 2027-01-01 · no tool calling")
    );
    assert_eq!(constrained.service_tiers[0].name, "Ultra Fast");
}
