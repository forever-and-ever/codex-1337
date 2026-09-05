//! Session-local questions. Replies use normal user input, never a synthetic tool response.

use std::collections::HashSet;

use codex_app_server_protocol::ToolRequestUserInputOption;
use codex_app_server_protocol::ToolRequestUserInputParams;
use codex_app_server_protocol::ToolRequestUserInputQuestion;
use codex_app_server_protocol::ToolRequestUserInputResponse;
use codex_protocol::items::AgentMessageDelivery;
use codex_protocol::items::AgentMessageItem;
use codex_protocol::items::AsyncUserInputQuestion;

use super::ChatWidget;
use super::UserMessageHistoryRecord;

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct AsyncQuestions {
    pending: Vec<ToolRequestUserInputQuestion>,
    // Completed notifications can be delivered again after reconnect or turn completion.
    seen: HashSet<String>,
}

impl AsyncQuestions {
    pub(crate) fn receive(&mut self, id: &str, questions: &[AsyncUserInputQuestion]) {
        for (index, question) in questions.iter().enumerate() {
            let id = format!("{id}:{index}");
            if !self.seen.insert(id.clone()) {
                continue;
            }
            self.pending.push(ToolRequestUserInputQuestion {
                id,
                header: "Question".to_string(),
                question: question.title.clone(),
                is_other: true,
                is_secret: false,
                options: question.options.as_ref().map(|options| {
                    options
                        .iter()
                        .map(|label| ToolRequestUserInputOption {
                            label: label.clone(),
                            description: String::new(),
                        })
                        .collect()
                }),
            });
        }
    }

    // Retain live questions buffered while another thread was selected, without
    // reviving questions answered or dismissed in this view.
    pub(crate) fn merge_unseen(&mut self, other: &Self) {
        self.pending.extend(
            other
                .pending
                .iter()
                .filter(|q| !self.seen.contains(&q.id))
                .cloned(),
        );
        self.seen.extend(other.seen.iter().cloned());
    }
}

impl ChatWidget {
    pub(crate) fn capture_async_questions(&self) -> AsyncQuestions {
        self.input_queue.async_questions.clone()
    }

    pub(crate) fn merge_async_questions(&mut self, questions: &AsyncQuestions) {
        self.input_queue.async_questions.merge_unseen(questions);
        self.refresh_async_questions();
    }

    pub(super) fn receive_async_questions(&mut self, item: &AgentMessageItem) {
        if item.delivery == Some(AgentMessageDelivery::Async)
            && let Some(questions) = &item.questions
        {
            self.input_queue
                .async_questions
                .receive(&item.id, questions);
        }
        self.refresh_async_questions();
    }

    pub(super) fn refresh_async_questions(&mut self) {
        let pending = &self.input_queue.async_questions.pending;
        self.bottom_pane
            .set_pending_questions(pending.len(), pending.first().map(|q| q.question.clone()));
    }

    pub(super) fn open_async_questions(&mut self) {
        let Some(thread_id) = self.thread_id else {
            return;
        };
        if self.input_queue.async_questions.pending.is_empty() {
            self.add_info_message("No pending questions.".to_string(), /*hint*/ None);
            return;
        }
        self.bottom_pane.open_async_questions(
            thread_id,
            ToolRequestUserInputParams {
                thread_id: thread_id.to_string(),
                turn_id: String::new(),
                item_id: String::new(),
                questions: self.input_queue.async_questions.pending.clone(),
                is_blocking: true, // Picker must never auto-submit; this does not block the running agent.
                auto_resolution_ms: None,
            },
        );
    }

    pub(crate) fn answer_async_questions(&mut self, response: ToolRequestUserInputResponse) {
        let pending = &self.input_queue.async_questions.pending;
        let text = pending
            .iter()
            .filter_map(|question| {
                let answer = response.answers.get(&question.id)?;
                (!answer.answers.is_empty()).then(|| {
                    format!(
                        "In reply to {:?}:\n{}",
                        question.question,
                        answer.answers.join("\n")
                    )
                })
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        if !text.is_empty()
            && !self.submit_user_message_with_history_record(
                text.into(),
                UserMessageHistoryRecord::UserMessageText,
            )
        {
            self.add_error_message(
                "Answer was not submitted; the questions remain pending.".to_string(),
            );
            return;
        }
        self.input_queue
            .async_questions
            .pending
            .retain(|q| !response.answers.contains_key(&q.id));
        self.refresh_async_questions();
    }
}
