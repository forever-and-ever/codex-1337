//! Response-order barriers, independent of the operation concurrency gate.
//!
//! Admission is registered synchronously as response items arrive. Completion is
//! published by the dispatch task, never by the response collector (which drains
//! only after sampling ends). Dropping a dispatch fails its dependents closed.

use std::sync::Arc;
use std::sync::Mutex;

use codex_tools::FunctionCallError;
use codex_tools::JsonSchema;
use codex_tools::ResponsesApiTool;
use codex_tools::ToolExecutor;
use codex_tools::ToolName;
use codex_tools::ToolSpec;
use tokio::sync::watch;

use super::context::FunctionToolOutput;
use super::context::ToolInvocation;
use super::registry::CoreToolRuntime;
use super::router::ToolCall;

type Verdict = Result<(), String>;
type Receipt = watch::Receiver<Option<Verdict>>;

#[derive(Default)]
pub(crate) struct ResponseBatch(Mutex<Stage>);

#[derive(Default)]
struct Stage {
    gate: Option<Receipt>,
    pending: Vec<Receipt>,
}

pub(super) struct Admission {
    requirements: Vec<Receipt>,
    completion: watch::Sender<Option<Verdict>>,
    call_id: String,
}

impl ResponseBatch {
    pub(super) fn admit(&self, call: &ToolCall) -> Admission {
        let mut stage = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (completion, receipt) = watch::channel(None);
        let mut requirements = stage.gate.iter().cloned().collect::<Vec<_>>();
        if is_fence(&call.tool_name) {
            requirements.append(&mut stage.pending);
            stage.gate = Some(receipt);
        } else {
            stage.pending.push(receipt);
        }
        Admission {
            requirements,
            completion,
            call_id: call.call_id.clone(),
        }
    }

    // Provider-side rejections never reach a handler, but remain prerequisites.
    pub(crate) fn reject(&self, item: &codex_protocol::models::ResponseItem) {
        let (_, receipt) = watch::channel(Some(Err("a preceding tool call was rejected".into())));
        let mut stage = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if matches!(item, codex_protocol::models::ResponseItem::RejectedToolCall { call, .. } if call.name() == "fence")
        {
            stage.pending.clear();
            stage.gate = Some(receipt);
        } else {
            stage.pending.push(receipt);
        }
    }
}

impl Admission {
    #[expect(
        clippy::expect_used,
        reason = "watch::wait_for returns a borrow satisfying Option::is_some"
    )]
    pub(super) async fn wait(&mut self) -> Result<(), FunctionCallError> {
        for requirement in &mut self.requirements {
            let verdict = requirement
                .wait_for(Option::is_some)
                .await
                .map_err(|_| {
                    FunctionCallError::RespondToModel(
                        "Skipped: prerequisite dispatch was lost".into(),
                    )
                })?
                .as_ref()
                .expect("waited for completion")
                .clone();
            verdict.map_err(|reason| {
                FunctionCallError::RespondToModel(format!("Skipped by fence: {reason}"))
            })?;
        }
        Ok(())
    }

    pub(super) fn finish(&self, verdict: Verdict) {
        self.completion.send_replace(Some(
            verdict.map_err(|reason| format!("{}: {reason}", self.call_id)),
        ));
    }
}

impl Drop for Admission {
    fn drop(&mut self) {
        if self.completion.borrow().is_none() {
            self.finish(Err("dispatch failed or was cancelled".into()));
        }
    }
}

/// One call and its nested work share an irreversible fence verdict. Catching a
/// JavaScript exception does not erase a failed prerequisite or background work.
#[derive(Clone, Default)]
pub(crate) struct Prerequisite(Arc<Mutex<CallState>>);

enum CallState {
    Foreground(usize),
    Failed,
    Detached,
}

impl Default for CallState {
    fn default() -> Self {
        Self::Foreground(0)
    }
}

pub(crate) struct CallMember(Option<Prerequisite>);

impl CallMember {
    #[expect(
        clippy::expect_used,
        reason = "consuming the member releases its sole live prerequisite"
    )]
    pub(crate) fn complete(mut self) {
        let call = self.0.take().expect("live nested call");
        let mut state = call
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let CallState::Foreground(pending) = &mut *state {
            *pending -= 1;
        }
    }
}

impl Drop for CallMember {
    fn drop(&mut self) {
        if let Some(call) = &self.0 {
            let _ = call.finish(/*success*/ false);
        }
    }
}

tokio::task_local! {
    static CURRENT_CALL: Prerequisite;
}

impl Prerequisite {
    pub(crate) fn current() -> Self {
        CURRENT_CALL.try_with(Clone::clone).unwrap_or_default()
    }

    pub(crate) fn enter<T>(&self, f: impl FnOnce() -> T) -> T {
        CURRENT_CALL.sync_scope(self.clone(), f)
    }

    pub(super) async fn run(
        &self,
        mut admission: Option<Admission>,
        future: impl Future<Output = Result<super::registry::AnyToolResult, FunctionCallError>>,
    ) -> Result<super::registry::AnyToolResult, FunctionCallError> {
        let result = async {
            if let Some(admission) = admission.as_mut() {
                admission.wait().await?;
            }
            CURRENT_CALL.scope(self.clone(), future).await
        }
        .await;
        let verdict = self.finish(
            result
                .as_ref()
                .is_ok_and(|result| result.result.success_for_logging()),
        );
        if let Some(admission) = &admission {
            admission.finish(verdict);
        }
        result
    }

    pub(crate) fn detached() {
        let _ = CURRENT_CALL.try_with(|call| {
            *call
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = CallState::Detached;
        });
    }

    pub(super) fn finish(&self, success: bool) -> Verdict {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !success && matches!(*state, CallState::Foreground(_)) {
            *state = CallState::Failed;
        }
        match *state {
            CallState::Foreground(0) => Ok(()),
            CallState::Foreground(_) => Err("nested work was not awaited".into()),
            CallState::Failed => Err("a prerequisite failed".into()),
            CallState::Detached => {
                Err("background or yielded work is not a foreground prerequisite".into())
            }
        }
    }

    pub(crate) fn member(&self) -> CallMember {
        let mut state = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let CallState::Foreground(pending) = &mut *state {
            *pending += 1;
        }
        CallMember(Some(self.clone()))
    }

    pub(crate) fn process_result(result: &super::context::ExecCommandToolOutput) {
        if result.process_id.is_some() {
            Self::detached();
        } else if result.exit_code.is_some_and(|code| code != 0) {
            let _ = Self::current().finish(/*success*/ false);
        }
    }
}

pub(crate) fn is_fence(name: &ToolName) -> bool {
    name.is_default_namespace() && name.name == "fence"
}

pub(crate) fn is_coordinator(name: &ToolName) -> bool {
    name.is_default_namespace() && matches!(name.name.as_str(), "exec" | "wait" | "fence")
}

pub(crate) struct FenceHandler;

impl ToolExecutor<ToolInvocation> for FenceHandler {
    fn tool_name(&self) -> ToolName {
        ToolName::plain("fence")
    }

    fn spec(&self) -> ToolSpec {
        ToolSpec::Function(ResponsesApiTool {
            name: "fence".into(),
            description: "Barrier for client-dispatched tool calls within this model response; provider-hosted tools are outside its scope. Calls before it must all finish successfully in the foreground before any call after it starts. Failure, rejection, cancellation, explicit backgrounding, or a yielded exec skips all following calls. Does not join background work or fence calls from earlier responses. Calls within each stage may run concurrently; use files for intermediate data. Takes {}.".into(),
            strict: true,
            defer_loading: None,
            parameters: JsonSchema::object(Default::default(), Some(Vec::new()), Some(false.into())),
            output_schema: None,
        })
    }

    fn handle<'a>(&'a self, invocation: ToolInvocation) -> codex_tools::ToolExecutorFuture<'a>
    where
        ToolInvocation: 'a,
    {
        Box::pin(async move {
            if matches!(
                invocation.source,
                super::router::ToolCallSource::CodeMode { .. }
            ) {
                return Err(FunctionCallError::RespondToModel(
                    "fence is a top-level response barrier, not a nested tool".into(),
                ));
            }
            let super::context::ToolPayload::Function { arguments } = invocation.payload else {
                return Err(FunctionCallError::RespondToModel("fence expects {}".into()));
            };
            #[derive(serde::Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Empty {}
            serde_json::from_str::<Empty>(&arguments)
                .map_err(|error| FunctionCallError::RespondToModel(error.to_string()))?;
            Ok(Box::new(FunctionToolOutput::from_text(
                "Fence passed".into(),
                Some(true),
            )) as Box<dyn codex_tools::ToolOutput>)
        })
    }

    fn supports_parallel_tool_calls(&self) -> bool {
        true
    }
}

impl CoreToolRuntime for FenceHandler {}
