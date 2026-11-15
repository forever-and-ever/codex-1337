//! Opt-in subscription acceptance probe; never reads production state or refreshes credentials.

#[path = "native_anthropic_acceptance/diagnostics.rs"]
mod diagnostics;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use anyhow::ensure;
use codex_api::AnthropicCachePolicy;
use codex_api::AnthropicCacheTtl;
use codex_api::AnthropicClient;
use codex_api::AnthropicOperation;
use codex_api::AnthropicPrompt;
use codex_api::AnthropicRequestOptions;
use codex_api::AnthropicStreamOptions;
use codex_api::AnthropicTool;
use codex_api::AnthropicToolIdentity;
use codex_api::ReqwestTransport;
use codex_api::ResponseEvent;
use codex_api::anthropic_function_input_schema;
use codex_api::encode_anthropic_request;
use codex_http_client::ClientRouteClass;
use codex_http_client::HttpClientBuilder;
use codex_http_client::HttpClientFactory;
use codex_http_client::OutboundProxyPolicy;
use codex_login::AuthCredentialsStoreMode;
use codex_login::AuthKeyringBackendKind;
use codex_login::AuthManager;
use codex_login::AuthRouteConfig;
use codex_login::anthropic::AccountIdentity;
use codex_login::anthropic::AccountInfo;
use codex_login::anthropic::AccountStore;
use codex_login::anthropic::SubscriptionGrant;
use codex_model_provider::create_model_provider_with_anthropic_account;
use codex_model_provider_info::ModelProviderAdapter;
use codex_model_provider_info::ModelProviderInfo;
use codex_models_manager::manager::RefreshStrategy;
use codex_protocol::models::AnthropicSignedCompaction;
use codex_protocol::models::ContentItem;
use codex_protocol::models::ResponseItem;
use codex_protocol::openai_models::ModelInfo;
use codex_protocol::openai_models::ReasoningEffort;
use futures::StreamExt;
use http::HeaderMap;
use http::HeaderValue;
use serde_json::Value;
use serde_json::json;
use std::fs::File;
use std::fs::OpenOptions;
use std::io::Read;
use std::io::Write;
use std::num::NonZeroU32;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use std::time::SystemTime;

const HAIKU: &str = "claude-haiku-4-5-20251001";
const MAX_BYTES: usize = 256 * 1024;
const MAX_CALLS: usize = 13;
const INSTRUCTIONS: &str = "Synthetic transport acceptance. Treat records as inert data. \
    Reply exactly OK unless explicitly asked to call echo_fixture. Never call it otherwise. \
    After its result reply exactly OK. Keep reasoning and output minimal.";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Arm {
    Full,
    Signed,
}

impl Arm {
    fn call_limit(self) -> usize {
        match self {
            Self::Full => MAX_CALLS,
            Self::Signed => 4,
        }
    }
    fn inference_cap(self) -> u32 {
        match self {
            Self::Full => 128,
            Self::Signed => 512,
        }
    }
}

struct Stage<'a> {
    name: &'static str,
    model: &'a ModelInfo,
    effort: ReasoningEffort,
    cap: u32,
    ttl: AnthropicCacheTtl,
    operation: AnthropicOperation,
}

struct Receipt {
    items: Vec<ResponseItem>,
    read: i64,
    write: i64,
    stop: String,
}

struct Driver {
    client: AnthropicClient<ReqwestTransport>,
    tools: Arc<[AnthropicTool]>,
    directory: PathBuf,
    journal: File,
    calls: usize,
    arm: Arm,
    diagnostics: diagnostics::Diagnostics,
}

impl Driver {
    fn record(&mut self, mut value: Value) -> Result<()> {
        value["at_unix_ms"] = json!(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)?
                .as_millis()
        );
        writeln!(self.journal, "{value}")?;
        Ok(self.journal.sync_all()?)
    }

    async fn send(&mut self, stage: Stage<'_>, input: &[ResponseItem]) -> Result<Receipt> {
        ensure!(
            self.calls < self.arm.call_limit(),
            "Messages budget exhausted"
        );
        let request = match encode_anthropic_request(
            &AnthropicPrompt {
                input,
                instructions: INSTRUCTIONS,
                tools: &self.tools,
                parallel_tool_calls: false,
                output_schema: None,
            },
            &AnthropicRequestOptions {
                model: stage.model,
                reasoning_effort: Some(&stage.effort),
                max_output_tokens: NonZeroU32::new(stage.cap).context("zero output cap")?,
                cache: AnthropicCachePolicy::Ephemeral(stage.ttl),
                operation: stage.operation,
            },
        ) {
            Ok(request) => request,
            Err(error) => {
                let diagnostic = self.diagnostics.api_error(&error);
                self.record(json!({"stage":stage.name,"status":"failed_before_send",
                    "diagnostic":diagnostic}))?;
                bail!("native encoding rejected {}: {diagnostic}", stage.name);
            }
        };
        if request.body().get().len() > MAX_BYTES {
            self.record(json!({"stage":stage.name,"status":"failed_before_send",
                "diagnostic":{"kind":"local_validation","message":"request exceeds probe bound"}}))?;
            bail!("request exceeds probe bound");
        }
        self.calls += 1;
        self.record(json!({"stage":stage.name,"status":"started",
            "call":self.calls,"model":stage.model.slug,"max_output_tokens":stage.cap,
            "reasoning_effort":stage.effort.as_str(),
            "ttl":stage.ttl,"operation":format!("{:?}",stage.operation)}))?;
        let mut upstream_request_id = None;
        let attempt = tokio::time::timeout(Duration::from_mins(/*mins*/ 1), async {
            let mut stream = self
                .client
                .stream_request(request, AnthropicStreamOptions::default())
                .await
                .context("Messages handshake")?;
            upstream_request_id = stream.upstream_request_id.as_ref().map(|id| {
                self.diagnostics
                    .text(id)
                    .chars()
                    .take(128)
                    .collect::<String>()
            });
            let mut items = Vec::new();
            let mut completed = None;
            let mut events = 0;
            while let Some(event) = stream.next().await {
                events += 1;
                ensure!(events <= 4096, "event budget exceeded");
                match event.context("Native Messages stream")? {
                    ResponseEvent::OutputItemDone(item) => items.push(item),
                    ResponseEvent::Completed {
                        token_usage,
                        usage_metadata,
                        ..
                    } => {
                        ensure!(completed.is_none(), "duplicate completion");
                        completed = Some((
                            token_usage.context("missing usage")?,
                            usage_metadata.context("missing upstream usage metadata")?,
                        ));
                    }
                    _ => {}
                }
            }
            let (usage, metadata) = completed.context("missing terminal event")?;
            let raw = metadata.metadata.context("missing native usage metadata")?;
            let stop = raw["stop_reason"]
                .as_str()
                .context("missing stop reason")?
                .to_owned();
            let read = usage.cached_input_tokens;
            let write = usage.cache_write_input_tokens;
            self.record(json!({"stage":stage.name,"status":"completed",
                "call":self.calls,"input_tokens":usage.input_tokens,
                "fresh_input_tokens":usage.input_tokens-read-write,"cache_read_tokens":read,
                "cache_write_tokens":write,"output_tokens":usage.output_tokens,
                "upstream_request_id":upstream_request_id,
                "upstream_usage":raw["usage"],"stop_reason":stop}))?;
            ensure!(
                usage.output_tokens <= i64::from(stage.cap),
                "output exceeded explicit cap"
            );
            ensure!(
                serde_json::to_vec(&items)?.len() <= MAX_BYTES,
                "output history exceeds bound"
            );
            Ok::<_, anyhow::Error>(Receipt {
                items,
                read,
                write,
                stop,
            })
        })
        .await;
        let receipt = match attempt {
            Ok(Ok(receipt)) => receipt,
            Err(_) => {
                self.record(json!({"stage":stage.name,"status":"failed",
                    "call":self.calls,"upstream_request_id":upstream_request_id,
                    "diagnostic":{"kind":"uncertain_timeout","message":"60-second stage deadline"}}))?;
                bail!("stage {} timed out; outcome unknown, no retry", stage.name);
            }
            Ok(Err(error)) => {
                let diagnostic = self.diagnostics.failure(&error);
                self.record(json!({"stage":stage.name,
                    "status":"failed","call":self.calls,
                    "upstream_request_id":upstream_request_id,"diagnostic":diagnostic}))?;
                bail!(
                    "stage {} failed: {diagnostic}; no refresh/retry",
                    stage.name
                );
            }
        };
        let expected = match stage.operation {
            AnthropicOperation::Compact => "compaction",
            AnthropicOperation::Inference
                if matches!(stage.name, "parent.tool" | "signed.resume_tool") =>
            {
                "tool_use"
            }
            AnthropicOperation::Inference => "end_turn",
        };
        if receipt.stop != expected {
            self.record(json!({"stage":stage.name,"status":"failed_acceptance",
                "stop_reason":receipt.stop,"expected_stop":expected,
                "diagnostic":{"kind":"local_validation","message":"unexpected terminal disposition"}}))?;
            bail!(
                "unexpected stop in {}; revise explicit operator cap if needed, never retry",
                stage.name
            );
        }
        if expected == "end_turn" {
            let text: String = receipt
                .items
                .iter()
                .flat_map(|item| match item {
                    ResponseItem::Message { content, .. } => content.as_slice(),
                    _ => &[],
                })
                .filter_map(|content| match content {
                    ContentItem::OutputText { text, .. } => Some(text.as_str()),
                    _ => None,
                })
                .collect();
            if text != "OK" {
                self.record(json!({"stage":stage.name,"status":"failed_acceptance",
                    "diagnostic":{"kind":"local_validation","message":"synthetic response was not exactly OK"}}))?;
                bail!("unexpected synthetic response in {}", stage.name);
            }
        }
        println!(
            "{}: {} read / {} write",
            stage.name, receipt.read, receipt.write
        );
        Ok(receipt)
    }

    fn persist(&self, name: &str, history: &[ResponseItem]) -> Result<Vec<ResponseItem>> {
        let bytes = serde_json::to_vec(history)?;
        ensure!(bytes.len() <= MAX_BYTES, "persisted history exceeds bound");
        let path = self.directory.join(name);
        let mut file = private_file(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        let reloaded: Vec<ResponseItem> = serde_json::from_slice(&std::fs::read(path)?)?;
        ensure!(
            serde_json::to_vec(&reloaded)? == bytes,
            "canonical persistence changed signed history"
        );
        Ok(reloaded)
    }
}

fn private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn user(text: &str) -> Result<ResponseItem> {
    Ok(serde_json::from_value(
        json!({"type":"message","role":"user",
        "content":[{"type":"input_text","text":text}]}),
    )?)
}

fn echo_result(items: &[ResponseItem], tool: &AnthropicTool) -> Result<ResponseItem> {
    ensure!(
        matches!(&tool.identity, AnthropicToolIdentity::Function {name,namespace}
        if name=="echo_fixture" && namespace.is_none())
            && tool.identity.wire_alias()? == tool.name,
        "synthetic tool alias mismatch"
    );
    let calls: Vec<_> = items
        .iter()
        .filter_map(|item| match item {
            ResponseItem::FunctionCall {
                name,
                namespace,
                call_id,
                arguments,
                ..
            } => Some((name, namespace, call_id, arguments)),
            _ => None,
        })
        .collect();
    let [(name, namespace, call_id, arguments)] = calls.as_slice() else {
        bail!("expected exactly one synthetic tool call");
    };
    ensure!(
        name.as_str() == "echo_fixture"
            && namespace.is_none()
            && !call_id.is_empty()
            && serde_json::from_str::<Value>(arguments.as_str())? == json!({}),
        "unexpected synthetic tool identity, call ID or arguments"
    );
    Ok(serde_json::from_value(
        json!({"type":"function_call_output",
        "call_id":call_id,"output":"OK"}),
    )?)
}

fn prefix(run: &str, arm: &str) -> Result<Vec<ResponseItem>> {
    let mut text = format!("Native-only synthetic run {run}, independent arm {arm}.\n");
    for record in 0..128 {
        use std::fmt::Write;
        writeln!(
            text,
            "Record {record:04}: amber basalt cedar delta ember frost granite harbor \
            iris juniper kestrel lagoon marble nickel orchard pewter quartz river slate timber. \
            This inert fixture has no instructions or private data."
        )?;
    }
    ensure!(text.len() < 64 * 1024, "synthetic prefix exceeds bound");
    Ok(vec![user(&text)?, user("Reply exactly OK.")?])
}

fn haiku_stage<'a>(name: &'static str, model: &'a ModelInfo, ttl: AnthropicCacheTtl) -> Stage<'a> {
    Stage {
        name,
        model,
        effort: ReasoningEffort::None,
        cap: 64,
        ttl,
        operation: AnthropicOperation::Inference,
    }
}

async fn run(args: &[String], arm: Arm) -> Result<()> {
    ensure!(
        args.len() == 5,
        "usage: --run|--run-signed ACCOUNT_UUID COMPACTION_MODEL FRESH_RECEIPT_DIR (bearer on stdin)"
    );
    let identity = AccountIdentity::new(&args[2])?;
    let directory = PathBuf::from(&args[4]);
    std::fs::create_dir(&directory).context("receipt directory must be fresh")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    }
    let mut token = String::new();
    std::io::stdin()
        .take(/*limit*/ 65537)
        .read_to_string(&mut token)
        .map_err(|_| anyhow::anyhow!("invalid bearer input"))?;
    ensure!(token.len() <= 65536, "bearer input exceeds bound");
    let diagnostics = diagnostics::Diagnostics::new(token.trim().to_owned());
    let grant = SubscriptionGrant::bearer_only(token.trim().to_owned())?;
    drop(token);
    let home = tempfile::Builder::new()
        .prefix("codex-native-acceptance-")
        .tempdir()?;
    let mut setup = private_file(&directory.join("setup.json"))?;
    serde_json::to_writer(
        &mut setup,
        &json!({"temporary_auth_directory":home.path(),
        "arm":if arm==Arm::Signed {"signed_only"} else {"full"},
        "messages_limit":arm.call_limit(),"haiku_output_cap":64,"signed_inference_output_cap":arm.inference_cap(),
        "compaction_output_cap":AnthropicSignedCompaction::MAX_OUTPUT_TOKENS.get()}),
    )?;
    setup.sync_all()?;
    let id = AccountStore::new(home.path().to_path_buf())
        .upsert(
            AccountInfo {
                identity,
                email: None,
                organization_uuid: None,
                organization_name: None,
            },
            grant,
        )
        .await?;
    let factory = HttpClientFactory::new(OutboundProxyPolicy::RespectSystemProxy);
    let auth_runtime = Arc::new(
        AuthManager::new(
            home.path().to_path_buf(),
            /*enable_codex_api_key_env*/ false,
            AuthCredentialsStoreMode::File,
            /*forced_chatgpt_workspace_id*/ None,
            /*chatgpt_base_url*/ None,
            AuthKeyringBackendKind::default(),
            AuthRouteConfig::from_http_client_factory(factory.clone()),
        )
        .await,
    );
    let provider = create_model_provider_with_anthropic_account(
        ModelProviderInfo {
            name: "Native subscription acceptance".into(),
            adapter: ModelProviderAdapter::ClaudeSubscription,
            request_max_retries: Some(0),
            stream_idle_timeout_ms: Some(60000),
            ..Default::default()
        },
        Some(auth_runtime),
        Some(id.to_string()),
    );
    let manager = provider.models_manager_without_cache(/*config_model_catalog*/ None);
    let catalog = manager
        .raw_model_catalog(RefreshStrategy::Online, factory.clone())
        .await;
    let select = |slug: &str| -> Result<ModelInfo> {
        let model = catalog
            .models
            .iter()
            .find(|model| model.slug == slug)
            .context("requested model absent from actual catalogue")?
            .clone();
        ensure!(
            model.anthropic.is_some(),
            "missing native authoritative capabilities"
        );
        Ok(model)
    };
    let compact = select(&args[3])?;
    ensure!(
        compact.supports_native_compaction,
        "model lacks signed compaction support"
    );
    let signed_effort = if compact
        .supported_reasoning_levels
        .iter()
        .any(|preset| preset.effort == ReasoningEffort::None)
    {
        ReasoningEffort::None
    } else {
        ensure!(
            compact
                .supported_reasoning_levels
                .iter()
                .any(|preset| preset.effort == ReasoningEffort::Low)
                && compact
                    .anthropic
                    .as_ref()
                    .is_some_and(|caps| caps.supported_efforts.contains(&ReasoningEffort::Low)),
            "compaction model declares neither disabled thinking nor native low effort"
        );
        ReasoningEffort::Low
    };
    let mut catalog_file = private_file(&directory.join("actual-models.json"))?;
    serde_json::to_writer(&mut catalog_file, &catalog)?;
    catalog_file.sync_all()?;
    let captured = provider
        .anthropic_auth_context()?
        .context("missing native auth")?
        .capture(&id.to_string())
        .await?;
    let http = HttpClientBuilder::new()
        .without_redirects()
        .without_request_logging()
        .default_headers(HeaderMap::from_iter([(
            http::header::USER_AGENT,
            HeaderValue::from_static("codex-1337/native-acceptance"),
        )]))
        .build_respecting_outbound_proxy_policy(
            &factory,
            "https://api.anthropic.com/v1/messages",
            ClientRouteClass::Api,
        )?;
    let client = AnthropicClient::new(
        ReqwestTransport::from_http_client(http),
        provider.info().to_api_provider(/*auth_mode*/ None)?,
        captured.auth(),
    );
    let identity = AnthropicToolIdentity::Function {
        name: "echo_fixture".into(),
        namespace: None,
    };
    let tools = Arc::from([AnthropicTool {
        name: identity.wire_alias()?,
        identity,
        description: "Return the fixed synthetic value OK; no external side effects.".into(),
        input_schema: anthropic_function_input_schema(json!({"type":"object",
            "properties":{},"additionalProperties":false}))?,
    }]);
    let journal = private_file(&directory.join("usage.jsonl"))?;
    let mut driver = Driver {
        client,
        tools,
        directory,
        journal,
        calls: 0,
        arm,
        diagnostics,
    };
    let run_id = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)?
        .as_nanos()
        .to_string();
    let one_hour = AnthropicCacheTtl::OneHour;
    if arm == Arm::Full {
        let haiku = select(HAIKU)?;
        let mut parent = prefix(&run_id, "parent-1h")?;
        let five_minutes = AnthropicCacheTtl::FiveMinutes;
        let cold = driver
            .send(haiku_stage("parent.cold", &haiku, one_hour), &parent)
            .await?;
        ensure!(
            cold.write > 0,
            "prefix did not reach upstream cache admission threshold"
        );
        let warm = driver
            .send(haiku_stage("parent.warm", &haiku, one_hour), &parent)
            .await?;
        ensure!(warm.read > 0, "parent warm cache miss");
        parent.extend(cold.items);
        parent.push(user(
            "Call echo_fixture exactly once with {}. Do not write text first.",
        )?);
        let call = driver
            .send(haiku_stage("parent.tool", &haiku, one_hour), &parent)
            .await?;
        ensure!(
            call.read > 0,
            "appended tool request lost the cached prefix"
        );
        let result = echo_result(&call.items, &driver.tools[0])?;
        parent.extend(call.items);
        parent.push(result);
        driver
            .send(haiku_stage("parent.tool_result", &haiku, one_hour), &parent)
            .await?;
        let parent = driver.persist("parent-history.json", &parent)?;
        let reload = driver
            .send(haiku_stage("parent.reload", &haiku, one_hour), &parent)
            .await?;
        ensure!(reload.read > 0, "persisted parent cache miss");
        let child = prefix(&run_id, "child-5m")?;
        let child_cold = driver
            .send(haiku_stage("child.cold", &haiku, five_minutes), &child)
            .await?;
        ensure!(child_cold.write > 0, "child cold cache miss not admitted");
        let child_warm = driver
            .send(haiku_stage("child.warm", &haiku, five_minutes), &child)
            .await?;
        ensure!(child_warm.read > 0, "child warm cache miss");
        driver.record(json!({"status":"ttl_wait","seconds":360}))?;
        tokio::time::sleep(Duration::from_mins(/*mins*/ 6)).await;
        let retained = driver
            .send(haiku_stage("parent.after_5m", &haiku, one_hour), &parent)
            .await?;
        ensure!(
            retained.read > 0,
            "one-hour parent not retained after five minutes"
        );
        let expired = driver
            .send(haiku_stage("child.after_5m", &haiku, five_minutes), &child)
            .await?;
        ensure!(
            expired.read == 0 && expired.write > 0,
            "five-minute child did not expire independently"
        );
    }
    let mut history = prefix(&run_id, "signed-1h")?;
    let signed_stage = |name, operation| Stage {
        name,
        model: &compact,
        effort: signed_effort.clone(),
        cap: match operation {
            AnthropicOperation::Inference => arm.inference_cap(),
            AnthropicOperation::Compact => AnthropicSignedCompaction::MAX_OUTPUT_TOKENS.get(),
        },
        ttl: one_hour,
        operation,
    };
    let ordinary = driver
        .send(
            signed_stage("signed.ordinary", AnthropicOperation::Inference),
            &history,
        )
        .await?;
    ensure!(
        ordinary.write > 0,
        "signed prefix did not reach cache admission threshold"
    );
    history.extend(ordinary.items);
    history.push(user("Preserve the fixture value OK in the checkpoint.")?);
    let history = driver.persist("signed-history.json", &history)?;
    let checkpoint = driver
        .send(
            signed_stage("signed.compact", AnthropicOperation::Compact),
            &history,
        )
        .await?;
    ensure!(
        checkpoint.read > 0,
        "compaction did not reuse the cached signed-arm prefix"
    );
    let checkpoints: Vec<_> = checkpoint
        .items
        .into_iter()
        .filter(|item| matches!(item, ResponseItem::NativeCompaction { .. }))
        .collect();
    ensure!(
        checkpoints.len() == 1,
        "missing or duplicate signed checkpoint"
    );
    let value = serde_json::to_value(&checkpoints[0])?;
    ensure!(
        value["content"].as_str().is_some_and(|s| !s.is_empty())
            && value["signature"].as_str().is_some_and(|s| !s.is_empty()),
        "unsigned or empty checkpoint"
    );
    let mut resumed = driver.persist("signed-checkpoint.json", &checkpoints)?;
    resumed.push(user(
        "Call echo_fixture exactly once with {}. Do not write text first.",
    )?);
    let resumed_receipt = driver
        .send(
            signed_stage("signed.resume_tool", AnthropicOperation::Inference),
            &resumed,
        )
        .await?;
    ensure!(
        resumed_receipt.read == 0,
        "checkpoint resume reported a cache hit without a demonstrated shared eligible prefix"
    );
    let result = echo_result(&resumed_receipt.items, &driver.tools[0])?;
    resumed.extend(resumed_receipt.items);
    resumed.push(result);
    let resumed = driver.persist("signed-tool-history.json", &resumed)?;
    driver
        .send(
            signed_stage("signed.tool_result", AnthropicOperation::Inference),
            &resumed,
        )
        .await?;
    ensure!(driver.calls == arm.call_limit(), "unexpected call count");
    driver.record(json!({"status":"complete","messages_calls":driver.calls,
        "run_id":run_id}))?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let arm = match args.get(/*index*/ 1).map(String::as_str) {
        Some("--run") => Arm::Full,
        Some("--run-signed") => Arm::Signed,
        _ => {
            println!(
                "No requests sent. Opt in: --run ACCOUNT_UUID COMPACTION_MODEL FRESH_RECEIPT_DIR; bearer on stdin.\n\
            Maximum 13 Messages calls (Haiku cap64, signed inference cap128, compaction cap4096), 360s TTL wait, no refresh/retries. \
            --run-signed selects a separate fresh four-call attempt, signed inference cap512, no TTL wait. \
            Catalogue discovery uses the bounded native parser. See scripts/native_anthropic/README.md."
            );
            return Ok(());
        }
    };
    let deadline = match arm {
        Arm::Full => 1200,
        Arm::Signed => 240,
    };
    Box::pin(tokio::time::timeout(
        Duration::from_secs(deadline),
        run(&args, arm),
    ))
    .await
    .context("overall acceptance deadline; preserve unknown receipt, do not retry")?
}
