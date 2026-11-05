use anyhow::Result;
use codex_config::ConfigLayerStack;
use codex_core::ForkSnapshot;
use codex_core::TurnInputRequest;
use codex_core::config::Constrained;
use codex_exec_server::CreateDirectoryOptions;
use codex_features::Feature;
use codex_models_manager::model_info::model_info_from_slug;
use codex_protocol::config_types::ApprovalsReviewer;
use codex_protocol::models::PermissionProfile;
use codex_protocol::openai_models::ApprovalMessages;
use codex_protocol::openai_models::ModelMessages;
use codex_protocol::openai_models::ModelsResponse;
use codex_protocol::openai_models::PermissionMessages;
use codex_protocol::permissions::FileSystemAccessMode;
use codex_protocol::permissions::FileSystemPath;
use codex_protocol::permissions::FileSystemSandboxEntry;
use codex_protocol::permissions::NetworkSandboxPolicy;
use codex_protocol::protocol::AskForApproval;
use codex_protocol::protocol::EventMsg;
use codex_protocol::protocol::ThreadSettingsOverrides;
use codex_protocol::user_input::UserInput;
use core_test_support::responses::ResponsesRequest;
use core_test_support::responses::ev_completed;
use core_test_support::responses::ev_response_created;
use core_test_support::responses::mount_sse_once;
use core_test_support::responses::sse;
use core_test_support::responses::start_mock_server;
use core_test_support::skip_if_no_network;
use core_test_support::test_codex::executor_path_uri;
use core_test_support::test_codex::test_codex;
use core_test_support::wait_for_event;
use pretty_assertions::assert_eq;
use serde_json::json;
use std::collections::HashSet;
use test_case::test_case;

const REQUEST_TOOL_BODY: &str = "The built-in `request_permissions` tool is available in this session. Invoke it when you need to request additional `network` or `file_system` permissions before later shell-like commands need them. Request only the specific permissions required for the task.";
const AUTO_REVIEW_BODY: &str = "`approvals_reviewer` is `auto_review`: Sandbox escalations with require_escalated will be reviewed for compliance with the policy. If a rejection happens, you should proceed only with a materially safer alternative, or inform the user of the risk and send a final message to ask for approval.";

fn permissions_texts(request: &ResponsesRequest) -> Vec<String> {
    request
        .message_input_texts("developer")
        .into_iter()
        .filter(|text| {
            text.contains("<permissions instructions>")
                || text.starts_with("<instruction_diff>\nApply this update to the previously supplied permissions instructions;")
        })
        .map(|text| text.replace("\r\n", "\n"))
        .collect()
}

fn permissions_annotations(
    request: &ResponsesRequest,
) -> Vec<(serde_json::Value, serde_json::Value, serde_json::Value)> {
    request
        .input()
        .into_iter()
        .flat_map(|item| {
            item["content"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
                .filter(|(_, content)| {
                    content["text"]
                        .as_str()
                        .is_some_and(|text| {
                            text.contains("<permissions instructions>")
                                || text.starts_with("<instruction_diff>\nApply this update to the previously supplied permissions instructions;")
                        })
                })
                .map(|(index, _)| {
                    (
                        item["role"].clone(),
                        item["internal_chat_message_metadata_passthrough"]["content_item_kinds"]
                            [index]
                            .clone(),
                        item["internal_chat_message_metadata_passthrough"]["content_item_metadata"]
                            [index]
                            .clone(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect::<Vec<_>>()
}

fn model_with_approval_messages(
    slug: &str,
    on_request: &str,
    on_request_auto_review: &str,
) -> codex_protocol::openai_models::ModelInfo {
    let mut model = model_info_from_slug(slug);
    model.model_messages = Some(ModelMessages {
        approvals: Some(ApprovalMessages {
            on_request: Some(on_request.to_string()),
            on_request_auto_review: Some(on_request_auto_review.to_string()),
            ..Default::default()
        }),
        ..Default::default()
    });
    model
}

fn model_with_permission_messages(
    slug: &str,
    permissions: PermissionMessages,
) -> codex_protocol::openai_models::ModelInfo {
    let mut model = model_info_from_slug(slug);
    model.model_messages = Some(ModelMessages {
        permissions: Some(permissions),
        ..Default::default()
    });
    model
}

async fn submit_text_turn(
    test: &core_test_support::test_codex::TestCodex,
    text: &str,
) -> Result<()> {
    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: text.to_string(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_approval_message_is_sent_in_initial_permissions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let model_slug = "catalog-approvals-model";
    let override_instructions = "literal {{ personality }} override";
    let model = model_with_approval_messages(
        model_slug,
        "catalog user approval instructions",
        "catalog auto-review approval instructions",
    );
    let mut builder = test_codex()
        .with_model(model_slug)
        .with_config(move |config| {
            config.base_instructions = Some(override_instructions.to_string());
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.model_catalog = Some(ModelsResponse {
                models: vec![model],
            });
        });
    let test = builder.build_with_auto_env(&server).await?;

    submit_text_turn(&test, "hello").await?;

    let request = req.single_request();
    assert_eq!(request.instructions_text(), override_instructions);
    let permissions = permissions_texts(&request);
    assert_eq!(permissions.len(), 1);
    assert!(permissions[0].contains("catalog user approval instructions"));
    assert!(permissions[0].contains("Filesystem sandboxing defines"));
    assert!(!permissions[0].contains("How to request escalation"));
    Ok(())
}

#[derive(Clone, Copy)]
enum ApprovalInstructionCase {
    Present,
    Absent,
    Complete,
}

#[test_case(ApprovalInstructionCase::Present; "present_fields_use_model_wording")]
#[test_case(ApprovalInstructionCase::Absent; "absent_fields_preserve_bundled_instructions")]
#[test_case(ApprovalInstructionCase::Complete; "complete_approval_message_takes_precedence")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_approval_instruction_parts_are_sent_in_initial_permissions(
    case: ApprovalInstructionCase,
) -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let model_slug = "catalog-approvals-model";
    let heading =
        |value: &str| (!matches!(case, ApprovalInstructionCase::Absent)).then(|| value.to_string());
    let sandbox = PermissionMessages {
        read_only: Some(String::new()),
        ..Default::default()
    };
    let mut model = model_info_from_slug(model_slug);
    model.use_responses_lite = true;
    model.model_messages = Some(ModelMessages {
        approvals: Some(ApprovalMessages {
            on_request_auto_review: matches!(case, ApprovalInstructionCase::Complete)
                .then(|| "Complete catalog approval instructions".to_string()),
            request_permissions: heading("Catalog permission tool"),
            auto_review_suffix: heading("Catalog automatic review"),
            ..Default::default()
        }),
        permissions: Some(sandbox.clone()),
        ..Default::default()
    });
    let fallback_slug = "catalog-approvals-fallback-model";
    let mut fallback_model = model_with_permission_messages(fallback_slug, sandbox);
    fallback_model.use_responses_lite = true;
    let mut builder = test_codex()
        .with_model(model_slug)
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.approvals_reviewer = ApprovalsReviewer::AutoReview;
            config
                .permissions
                .set_permission_profile(PermissionProfile::read_only())
                .expect("read-only permissions should be allowed");
            config.config_layer_stack = ConfigLayerStack::default();
            for (feature, value) in [
                (Feature::ExecPermissionApprovals, true),
                (Feature::RequestPermissionsTool, true),
            ] {
                config
                    .features
                    .set_enabled(feature, value)
                    .expect("test feature setting should be allowed");
            }
            config.model_catalog = Some(ModelsResponse {
                models: vec![model, fallback_model],
            });
        });
    let test = builder.build_with_auto_env(&server).await?;
    submit_text_turn(&test, "hello").await?;

    let request = req.single_request();
    let bundled_rule = include_str!("../../../prompts/templates/permissions/approval_policy/on_request_rule_request_permission.md").replace("\r\n", "\n");
    let bundled_approval = format!(
        "{bundled_rule}\n\n# request_permissions Tool\n\n{REQUEST_TOOL_BODY}\n\n{AUTO_REVIEW_BODY}"
    );
    let expected = match case {
        ApprovalInstructionCase::Complete => "Complete catalog approval instructions".to_string(),
        ApprovalInstructionCase::Present => {
            format!(
                "{bundled_rule}\n\nCatalog permission tool\n\n{REQUEST_TOOL_BODY}\n\nCatalog automatic review\n\n{AUTO_REVIEW_BODY}"
            )
        }
        ApprovalInstructionCase::Absent => bundled_approval.clone(),
    };
    let first = format!("<permissions instructions>\n{expected}\n</permissions instructions>");
    let annotation = (
        json!("developer"),
        json!("permissions.instructions"),
        json!({"provenance": {"type": "harness", "hook": false}}),
    );
    assert_eq!(permissions_texts(&request), vec![first.clone()]);
    assert_eq!(permissions_annotations(&request), vec![annotation.clone()]);

    if matches!(case, ApprovalInstructionCase::Present) {
        let next = mount_sse_once(
            &server,
            sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
        )
        .await;
        core_test_support::submit_thread_settings(
            &test.codex,
            ThreadSettingsOverrides {
                model: Some(fallback_slug.to_string()),
                ..Default::default()
            },
        )
        .await?;
        submit_text_turn(&test, "Continue with the fallback model.").await?;
        let next = next.single_request();
        assert!(next.input().starts_with(&request.input()));
        let updates = permissions_texts(&next);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0], first);
        assert!(updates[1].starts_with("<instruction_diff>\n"));
        assert!(updates[1].contains("\n-Catalog permission tool\n+# request_permissions Tool\n"));
        assert!(updates[1].contains("\n-Catalog automatic review\n"));
        assert_eq!(
            permissions_annotations(&next),
            vec![
                annotation.clone(),
                (
                    json!("developer"),
                    json!("context.instruction_diff/permissions"),
                    annotation.2
                )
            ],
        );
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_change_appends_new_catalog_approval_message() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;
    let first_slug = "catalog-approvals-model-a";
    let second_slug = "catalog-approvals-model-b";
    let first = model_with_approval_messages(first_slug, "model A approvals", "model A auto");
    let second = model_with_approval_messages(second_slug, "model B approvals", "model B auto");
    let mut builder = test_codex()
        .with_model(first_slug)
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
            config.model_catalog = Some(ModelsResponse {
                models: vec![first, second],
            });
        });
    let test = builder.build(&server).await?;
    submit_text_turn(&test, "first").await?;

    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some(second_slug.to_string()),
            ..Default::default()
        },
    )
    .await?;
    submit_text_turn(&test, "second").await?;

    let permissions = permissions_texts(&req2.single_request());
    assert_eq!(permissions.len(), 2);
    assert!(
        permissions
            .last()
            .is_some_and(|text| text.contains("model B approvals"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_non_on_request_approval_messages_are_sent_in_initial_permissions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    for (approval_policy, approvals, expected, unexpected) in [
        (
            AskForApproval::Never,
            ApprovalMessages {
                never: Some("catalog never approval instructions".to_string()),
                ..Default::default()
            },
            "catalog never approval instructions",
            "Approval policy is currently never",
        ),
        (
            AskForApproval::UnlessTrusted,
            ApprovalMessages {
                unless_trusted: Some("catalog unless-trusted approval instructions".to_string()),
                ..Default::default()
            },
            "catalog unless-trusted approval instructions",
            "`approval_policy` is `unless-trusted`",
        ),
    ] {
        let server = start_mock_server().await;
        let req = mount_sse_once(
            &server,
            sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
        )
        .await;
        let model_slug = "catalog-non-on-request-approvals-model";
        let mut model = model_info_from_slug(model_slug);
        model.model_messages = Some(ModelMessages {
            approvals: Some(approvals),
            ..Default::default()
        });
        let mut builder = test_codex()
            .with_model(model_slug)
            .with_config(move |config| {
                config.permissions.approval_policy = Constrained::allow_any(approval_policy);
                config.model_catalog = Some(ModelsResponse {
                    models: vec![model],
                });
            });
        let test = builder.build(&server).await?;

        submit_text_turn(&test, "hello").await?;

        let permissions = permissions_texts(&req.single_request());
        assert_eq!(permissions.len(), 1);
        assert!(permissions[0].contains(expected));
        assert!(!permissions[0].contains(unexpected));
    }
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn catalog_permission_message_is_sent_initially_and_after_model_change() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;
    let first_slug = "catalog-permissions-model-a";
    let second_slug = "catalog-permissions-model-b";
    let first = model_with_permission_messages(
        first_slug,
        PermissionMessages {
            danger_full_access: None,
            workspace_write: None,
            read_only: Some("model A permissions: {{ network_access }}".to_string()),
            approved_prefixes: None,
            writable_root: None,
            writable_roots: None,
            ..Default::default()
        },
    );
    let second = model_with_permission_messages(
        second_slug,
        PermissionMessages {
            danger_full_access: None,
            workspace_write: None,
            read_only: Some("model B permissions: {{ network_access }}".to_string()),
            approved_prefixes: None,
            writable_root: None,
            writable_roots: None,
            ..Default::default()
        },
    );
    let mut builder = test_codex()
        .with_model(first_slug)
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::Never);
            config
                .permissions
                .set_permission_profile(PermissionProfile::read_only())
                .expect("read-only permission profile should be allowed");
            config.model_catalog = Some(ModelsResponse {
                models: vec![first, second],
            });
        });
    let test = builder.build(&server).await?;
    submit_text_turn(&test, "first").await?;

    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            model: Some(second_slug.to_string()),
            ..Default::default()
        },
    )
    .await?;
    submit_text_turn(&test, "second").await?;

    let permissions = permissions_texts(&req2.single_request());
    assert_eq!(permissions.len(), 2);
    assert!(permissions[0].contains("model A permissions: restricted"));
    assert!(
        permissions
            .last()
            .is_some_and(|text| text.contains("model B permissions: restricted"))
    );
    assert!(
        permissions
            .last()
            .is_some_and(|text| text.contains("Approval policy is currently never"))
    );
    assert!(
        !permissions
            .iter()
            .any(|text| text.contains("`sandbox_mode`"))
    );
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn empty_catalog_permission_message_preserves_approval_instructions() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let model_slug = "empty-catalog-permissions-model";
    let model = model_with_permission_messages(
        model_slug,
        PermissionMessages {
            danger_full_access: None,
            workspace_write: None,
            read_only: Some(String::new()),
            approved_prefixes: None,
            writable_root: None,
            writable_roots: None,
            ..Default::default()
        },
    );
    let mut builder = test_codex()
        .with_model(model_slug)
        .with_config(move |config| {
            config.permissions.approval_policy = Constrained::allow_any(AskForApproval::Never);
            config
                .permissions
                .set_permission_profile(PermissionProfile::read_only())
                .expect("read-only permission profile should be allowed");
            config.model_catalog = Some(ModelsResponse {
                models: vec![model],
            });
        });
    let test = builder.build(&server).await?;

    submit_text_turn(&test, "hello").await?;

    let permissions = permissions_texts(&req.single_request());
    assert_eq!(permissions.len(), 1);
    assert!(permissions[0].contains("Approval policy is currently never"));
    assert!(!permissions[0].contains("Filesystem sandboxing defines"));
    assert!(!permissions[0].contains("`sandbox_mode`"));
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_message_sent_once_on_start() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;

    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    });
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    assert_eq!(permissions_texts(&req.single_request()).len(), 1);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_message_added_on_override_change() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    });
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 1".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            approval_policy: Some(AskForApproval::Never),
            ..Default::default()
        },
    )
    .await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 2".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let permissions_1 = permissions_texts(&req1.single_request());
    let permissions_2 = permissions_texts(&req2.single_request());

    assert_eq!(permissions_1.len(), 1);
    assert_eq!(permissions_2.len(), 2);
    let unique = permissions_2.into_iter().collect::<HashSet<String>>();
    assert_eq!(unique.len(), 2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_message_not_added_when_no_change() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let mut builder = test_codex().with_config(move |config| {
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    });
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 1".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 2".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let permissions_1 = permissions_texts(&req1.single_request());
    let permissions_2 = permissions_texts(&req2.single_request());

    assert_eq!(permissions_1.len(), 1);
    assert_eq!(permissions_2.len(), 1);
    assert_eq!(permissions_1, permissions_2);

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permissions_message_omitted_when_disabled() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;

    let mut builder = test_codex().with_config(move |config| {
        config.include_permissions_instructions = false;
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    });
    let test = builder.build(&server).await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 1".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    core_test_support::submit_thread_settings(
        &test.codex,
        ThreadSettingsOverrides {
            approval_policy: Some(AskForApproval::Never),
            ..Default::default()
        },
    )
    .await?;

    test.codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 2".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&test.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    assert_eq!(
        permissions_texts(&req1.single_request()),
        Vec::<String>::new()
    );
    assert_eq!(
        permissions_texts(&req2.single_request()),
        Vec::<String>::new()
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_replays_permissions_messages() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let _req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;
    let req3 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-3"), ev_completed("resp-3")]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    });
    let initial = builder.build(&server).await?;

    initial
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 1".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    core_test_support::submit_thread_settings(
        &initial.codex,
        ThreadSettingsOverrides {
            approval_policy: Some(AskForApproval::Never),
            ..Default::default()
        },
    )
    .await?;

    initial
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 2".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let resumed = builder.restart(&server, &initial).await?;
    resumed
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "after resume".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&resumed.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let permissions = permissions_texts(&req3.single_request());
    assert_eq!(permissions.len(), 3);
    // TELOS(instruction-amendments): resume replays both directional amendments, not full copies.
    assert!(permissions[0].starts_with("<permissions instructions>"));
    assert!(permissions[1].starts_with("<instruction_diff>"));
    assert!(permissions[1].contains("+Approval policy is currently never."));
    assert!(permissions[2].starts_with("<instruction_diff>"));
    assert!(permissions[2].contains("-Approval policy is currently never."));
    assert!(permissions[2].contains("+# Escalation Requests"));

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resume_and_fork_append_permissions_messages() -> Result<()> {
    skip_if_no_network!(Ok(()));

    let server = start_mock_server().await;
    let _req1 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
    )
    .await;
    let req2 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
    )
    .await;
    let req3 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-3"), ev_completed("resp-3")]),
    )
    .await;
    let req4 = mount_sse_once(
        &server,
        sse(vec![ev_response_created("resp-4"), ev_completed("resp-4")]),
    )
    .await;

    let mut builder = test_codex().with_config(|config| {
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::OnRequest);
    });
    let initial = builder.build(&server).await?;
    let rollout_path = initial
        .session_configured
        .rollout_path
        .clone()
        .expect("rollout path");

    initial
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 1".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    core_test_support::submit_thread_settings(
        &initial.codex,
        ThreadSettingsOverrides {
            approval_policy: Some(AskForApproval::Never),
            ..Default::default()
        },
    )
    .await?;

    initial
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "hello 2".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&initial.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let permissions_base = permissions_texts(&req2.single_request());
    assert_eq!(permissions_base.len(), 2);

    builder = builder.with_config(|config| {
        config.permissions.approval_policy = Constrained::allow_any(AskForApproval::UnlessTrusted);
    });
    let resumed = builder.restart(&server, &initial).await?;
    resumed
        .codex
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "after resume".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&resumed.codex, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let permissions_resume = permissions_texts(&req3.single_request());
    assert_eq!(permissions_resume.len(), permissions_base.len() + 1);
    assert_eq!(
        &permissions_resume[..permissions_base.len()],
        permissions_base.as_slice()
    );
    assert!(!permissions_base.contains(permissions_resume.last().expect("new permissions")));

    let mut fork_config = initial.config.clone();
    fork_config.permissions.approval_policy = Constrained::allow_any(AskForApproval::UnlessTrusted);
    let forked = initial
        .thread_manager
        .fork_legacy_thread(
            ForkSnapshot::Interrupted,
            codex_core::StartThreadOptions::new(fork_config.clone()),
            rollout_path,
        )
        .await?;
    forked
        .thread
        .start_or_steer_turn(TurnInputRequest::user_input(vec![UserInput::Text {
            text: "after fork".into(),
            text_elements: Vec::new(),
        }]))
        .await?;
    wait_for_event(&forked.thread, |ev| matches!(ev, EventMsg::TurnComplete(_))).await;

    let permissions_fork = permissions_texts(&req4.single_request());
    assert_eq!(permissions_fork.len(), permissions_base.len() + 1);
    assert_eq!(
        &permissions_fork[..permissions_base.len()],
        permissions_base.as_slice()
    );
    let new_permissions = &permissions_fork[permissions_base.len()..];
    assert_eq!(new_permissions.len(), 1);
    assert_eq!(permissions_fork, permissions_resume);
    assert!(!permissions_base.contains(&new_permissions[0]));

    Ok(())
}
pub(super) async fn catalog_denied_reads_model_change_requests() -> Result<Vec<ResponsesRequest>> {
    let server = start_mock_server().await;
    core_test_support::mock_server::run(&[&server], async {
        let req1 = mount_sse_once(
            &server,
            sse(vec![ev_response_created("resp-1"), ev_completed("resp-1")]),
        )
        .await;
        let first_slug = "catalog-denied-reads-a";
        let second_slug = "catalog-denied-reads-b";
        let models = [(first_slug, "Model A"), (second_slug, "Model B")]
            .into_iter()
            .map(|(slug, label)| {
                let mut model = model_with_permission_messages(
                    slug,
                    PermissionMessages {
                        denied_reads: Some(format!("{label} denied reads:\n{{{{ denied_reads }}}}")),
                        ..Default::default()
                    },
                );
                model.use_responses_lite = true;
                model
            })
            .collect();
        let mut builder = test_codex()
            .with_model(first_slug)
            .with_workspace_setup(move |cwd, fs| async move {
                for path in ["catalog-denied", "catalog-secrets"] {
                    fs.create_directory(
                        &executor_path_uri(cwd.join(path))?,
                        CreateDirectoryOptions {
                            recursive: true,
                            follow_symlinks: true,
                        },
                        /*sandbox*/ None,
                    )
                    .await?;
                }
                Ok(())
            })
            .with_config(move |config| {
                config.base_instructions = Some("Help with the workspace.".to_string());
                // Match the canonical writable root so snapshot normalization recognizes the cwd.
                if !core_test_support::is_remote_test_environment() {
                    config.cwd = dunce::canonicalize(config.cwd.as_path())
                        .expect("test workspace should be canonicalizable")
                        .try_into()
                        .expect("canonical workspace path should be absolute");
                }
                config.permissions.approval_policy = Constrained::allow_any(AskForApproval::Never);
                let cwd = executor_path_uri(&config.cwd).expect("executor cwd URI");
                let mut file_system = PermissionProfile::workspace_write_with_path_uris(
                    &[],
                    NetworkSandboxPolicy::Restricted,
                    /*exclude_tmpdir_env_var*/ true,
                    /*exclude_slash_tmp*/ true,
                )
                .file_system_sandbox_policy();
                file_system.entries.extend([
                    FileSystemSandboxEntry::new(
                        cwd.join("catalog-denied")
                            .expect("executor denied path URI")
                            .into(),
                        FileSystemAccessMode::Deny,
                    ),
                    FileSystemSandboxEntry::new(
                        FileSystemPath::GlobPattern {
                            pattern: cwd
                                .join("catalog-secrets/*.key")
                                .expect("executor denied glob URI")
                                .inferred_native_path_string(),
                        },
                        FileSystemAccessMode::Deny,
                    ),
                ]);
                config
                    .permissions
                    .set_permission_profile(PermissionProfile::from_runtime_permissions(
                        &file_system,
                        NetworkSandboxPolicy::Restricted,
                    ))
                    .expect("workspace-write profile should be allowed");
                config.workspace_roots = vec![config.cwd.clone()];
                config
                    .permissions
                    .set_workspace_roots(config.workspace_roots.clone());
                config.config_layer_stack = ConfigLayerStack::default();
                config.model_catalog = Some(ModelsResponse { models });
            });
        let test = builder.build_with_auto_env(&server).await?;
        submit_text_turn(&test, "Inspect the writable workspace.").await?;
        core_test_support::submit_thread_settings(
            &test.codex,
            ThreadSettingsOverrides {
                model: Some(second_slug.to_string()),
                ..Default::default()
            },
        )
        .await?;
        let req2 = mount_sse_once(
            &server,
            sse(vec![ev_response_created("resp-2"), ev_completed("resp-2")]),
        )
        .await;
        submit_text_turn(&test, "Continue with the selected model.").await?;
        let req3 = mount_sse_once(
            &server,
            sse(vec![ev_response_created("resp-3"), ev_completed("resp-3")]),
        )
        .await;
        submit_text_turn(&test, "Continue without changing permissions.").await?;
        let requests = vec![
            req1.single_request(),
            req2.single_request(),
            req3.single_request(),
        ];
        let first = permissions_texts(&requests[0]);
        assert_eq!(first.len(), 1);
        let first = first[0].clone();
        let cwd = executor_path_uri(&test.config.cwd)?;
        let denied_path = cwd.join("catalog-denied")?.inferred_native_path_string();
        let denied_glob = cwd
            .join("catalog-secrets/*.key")?
            .inferred_native_path_string();
        assert!(first.ends_with(&format!(
            "\nModel A denied reads:\nThe active permission profile denies reading these paths/globs. Do not request escalation or additional permissions to read them; these denials are policy restrictions.\n- path `{denied_path}`\n- glob `{denied_glob}`\n</permissions instructions>"
        )));
        let updates = permissions_texts(&requests[1]);
        assert_eq!(updates.len(), 2);
        assert_eq!(updates[0], first);
        assert!(updates[1].starts_with("<instruction_diff>\n"));
        assert!(updates[1].contains("\n-Model A denied reads:\n+Model B denied reads:\n"));
        assert!(updates[1].contains(&format!("- path `{denied_path}`")));
        assert!(updates[1].contains(&format!("- glob `{denied_glob}`")));
        assert_eq!(
            permissions_texts(&requests[2]),
            permissions_texts(&requests[1])
        );
        for (request, changed) in requests.iter().zip([false, true, true]) {
            let annotation = (
                json!("developer"),
                json!("permissions.instructions"),
                json!({"provenance": {"type": "harness", "hook": false}}),
            );
            let mut expected = vec![annotation.clone()];
            if changed {
                expected.push((json!("developer"), json!("context.instruction_diff/permissions"), annotation.2));
            }
            assert_eq!(permissions_annotations(request), expected);
        }
        for pair in requests.windows(2) {
            assert!(pair[1].input().starts_with(&pair[0].input()));
        }
        Ok(requests)
    })
    .await
}
