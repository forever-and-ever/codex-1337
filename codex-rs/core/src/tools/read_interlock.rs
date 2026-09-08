//! Best-effort suppression of literal shell reads, anchored to retained model output.
//! Pending receipts contain hashes, never file bodies. Merely executing a read is
//! insufficient: its complete raw or JSON-escaped result must occur in history.
//!
//! Only completed, non-TTY literal `cat` and `sed -n 'N,Mp'` commands participate.
//! Shell execution is never skipped. `# force_reread` on its own first command
//! line bypasses suppression. Compaction invalidates all receipts.
//!
//! Audit: `<log_dir>/read-interlock/<thread_id>.jsonl`, one record per recognized
//! attempt, no paths or bodies. Group by `outcome` for suppression, exemption,
//! changed-content and override counts; `forced` also covers small forced reads.
//! Token estimates use the byte heuristic and tool budget, not billing/quota,
//! and precede any enclosing Code Mode output cap.

use crate::context_manager::ContextManager;
use crate::session::turn_context::TurnEnvironment;
use crate::tools::context::ExecCommandToolOutput;
use crate::unified_exec::UnifiedExecContext;
use codex_protocol::models::ResponseItem;
use codex_shell_command::bash::extract_bash_command;
use codex_shell_command::read_intent::ReadIntent;
use codex_shell_command::read_intent::ReadRange;
use codex_utils_output_truncation::approx_token_count;
use codex_utils_path_uri::PathUri;
use serde::Serialize;
use sha1::Digest;
use sha1::Sha1;
use std::collections::HashMap;
use std::sync::Mutex;
use tokio::io::AsyncWriteExt;

const MAX_RECEIPTS: usize = 256;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Key {
    environment: String,
    path: String,
    range: ReadRange,
}

struct Receipt {
    id: String,
    content_hash: [u8; 20],
    representations: [(usize, [u8; 20]); 2],
}

impl Receipt {
    fn observed(&self, history: &ContextManager) -> bool {
        let marker = format!("Read {} · ", self.id);
        history.raw_items().any(|item| {
            let output = match item {
                ResponseItem::FunctionCallOutput { output, .. }
                | ResponseItem::CustomToolCallOutput { output, .. } => output,
                _ => return false,
            };
            let Some(text) = output.body.to_text() else {
                return false;
            };
            text.match_indices(&marker).any(|(offset, _)| {
                self.representations.iter().any(|(length, hash)| {
                    text.as_bytes()
                        .get(offset..offset.saturating_add(*length))
                        .is_some_and(|bytes| digest(bytes) == *hash)
                })
            })
        })
    }
}

#[derive(Default)]
struct Bank {
    generation: u64,
    receipts: HashMap<Key, Receipt>,
}

#[derive(Default)]
struct ReadBank(Mutex<Bank>);

pub(crate) struct ReadAttempt {
    intent: ReadIntent,
    key: Option<Key>,
    generation: u64,
}

impl ReadAttempt {
    pub(crate) async fn failed(&self, context: &UnifiedExecContext) {
        audit(
            context,
            &Audit {
                turn_id: &context.step_context.turn.sub_id,
                read_id: None,
                outcome: "failed",
                forced: self.intent.forced,
                captured_bytes: 0,
                estimated_output_tokens_suppressed: 0,
            },
        )
        .await;
    }

    pub(crate) async fn prepare(
        command: &[String],
        context: &UnifiedExecContext,
        cwd: &PathUri,
        environment: &TurnEnvironment,
    ) -> Option<Self> {
        let (_, script) = extract_bash_command(command)?;
        let intent = ReadIntent::parse(script)?;
        let generation = context.session.clone_history().await.history_version();
        let path = cwd.join(&intent.path).ok()?;
        let sandbox = environment.sandbox_context(/*additional_permissions*/ None);
        let canonical = environment
            .environment
            .get_filesystem()
            .canonicalize(&path, Some(&sandbox))
            .await
            .ok();
        let key = canonical.map(|path| Key {
            environment: environment.selection.environment_id.clone(),
            path: path.to_string(),
            range: intent.range.clone(),
        });
        Some(Self {
            intent,
            key,
            generation,
        })
    }

    pub(crate) async fn finish(
        self,
        context: &UnifiedExecContext,
        output: &mut ExecCommandToolOutput,
    ) {
        let history = context.session.clone_history().await;
        let bank = context
            .session
            .services
            .thread_extension_data
            .get_or_init(ReadBank::default);
        let captured_bytes = output.raw_output.len();
        let (outcome, id, suppressed) = {
            let mut bank = bank
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            self.process(&mut bank, &history, output)
        };
        audit(
            context,
            &Audit {
                turn_id: &context.step_context.turn.sub_id,
                read_id: id.as_deref(),
                outcome,
                forced: self.intent.forced,
                captured_bytes,
                estimated_output_tokens_suppressed: suppressed,
            },
        )
        .await;
    }

    fn process(
        &self,
        bank: &mut Bank,
        history: &ContextManager,
        output: &mut ExecCommandToolOutput,
    ) -> (&'static str, Option<String>, usize) {
        if bank.generation != history.history_version() {
            bank.receipts.clear();
            bank.generation = history.history_version();
        }
        if output.exit_code != Some(0) || output.process_id.is_some() {
            return ("incomplete_or_failed", None, 0);
        }
        if output.output_omitted_bytes.is_some() {
            return ("capture_truncated", None, 0);
        }
        let Ok(content) = std::str::from_utf8(&output.raw_output) else {
            return ("non_utf8", None, 0);
        };
        let small = content.len() <= 1024 && content.lines().count() <= 10;
        let hash = digest(content.as_bytes());
        let key = self.key.as_ref();
        let previous = key
            .and_then(|key| bank.receipts.get(key))
            .filter(|receipt| receipt.observed(history));
        let label = self.intent.label(&self.intent.path);
        if !small
            && !self.intent.forced
            && self.generation == history.history_version()
            && let Some(previous) = previous.filter(|receipt| receipt.content_hash == hash)
        {
            let notice = format!("Unchanged {label} already delivered as {}", previous.id);
            let budget = crate::unified_exec::resolve_max_tokens(output.max_output_tokens)
                .min(output.truncation_policy.token_budget());
            let suppressed = approx_token_count(&output.truncated_output(budget))
                .saturating_sub(approx_token_count(&notice));
            output.raw_output = notice.into_bytes();
            output.original_token_count = None;
            return ("suppressed", Some(previous.id.clone()), suppressed);
        }
        let changed = previous.is_some_and(|receipt| receipt.content_hash != hash);
        let id = format!("R{}", &uuid::Uuid::new_v4().simple().to_string()[..12]);
        let text = format!("Read {id} · {label}\n{content}");
        if !small
            && self.generation == history.history_version()
            && let Some(key) = key
            && let Ok(escaped) = serde_json::to_string(&text)
        {
            // An output-size limit or a Code Mode cell may still hide this body.
            // It becomes eligible only when a later attempt observes it in history.
            let escaped = &escaped[1..escaped.len() - 1];
            if bank.receipts.len() >= MAX_RECEIPTS {
                bank.receipts.clear();
            }
            bank.receipts.insert(
                key.clone(),
                Receipt {
                    id: id.clone(),
                    content_hash: hash,
                    representations: [
                        (text.len(), digest(text.as_bytes())),
                        (escaped.len(), digest(escaped.as_bytes())),
                    ],
                },
            );
        }
        output.raw_output = text.into_bytes();
        let outcome = if small {
            "small_exempt"
        } else if self.intent.forced {
            "forced"
        } else if changed {
            "changed"
        } else if key.is_none() {
            "unresolved_path"
        } else {
            "pending_delivery"
        };
        (outcome, Some(id), 0)
    }
}

fn digest(bytes: &[u8]) -> [u8; 20] {
    Sha1::digest(bytes).into()
}

#[derive(Serialize)]
struct Audit<'a> {
    turn_id: &'a str,
    read_id: Option<&'a str>,
    outcome: &'a str,
    forced: bool,
    captured_bytes: usize,
    /// Byte-heuristic estimate under the tool budget, before any enclosing Code Mode
    /// output cap. Not tokenizer counts, billed tokens, or quota saved.
    estimated_output_tokens_suppressed: usize,
}

async fn audit(context: &UnifiedExecContext, event: &Audit<'_>) {
    let config = &context.step_context.turn.config;
    context.step_context.turn.session_telemetry.counter(
        "codex.read_interlock.attempt",
        1,
        &[("outcome", event.outcome)],
    );
    if config.ephemeral {
        return;
    }
    let result = async {
        let directory = config.log_dir.join("read-interlock");
        tokio::fs::create_dir_all(&directory).await?;
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        options.mode(0o600);
        let mut file = options
            .open(directory.join(format!("{}.jsonl", context.session.thread_id)))
            .await?;
        let mut record = serde_json::to_vec(event)?;
        record.push(b'\n');
        file.write_all(&record).await
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(%error, "cannot write read-interlock audit");
    }
}

#[cfg(test)]
#[path = "read_interlock_tests.rs"]
mod tests;
