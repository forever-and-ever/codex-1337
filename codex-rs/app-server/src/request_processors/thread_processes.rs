use super::*;
use codex_app_server_protocol::ThreadBackgroundTerminalsScope;

impl ThreadRequestProcessor {
    pub(super) async fn list_owned_background_terminals(
        &self,
        params: ThreadBackgroundTerminalsListParams,
    ) -> Result<ThreadBackgroundTerminalsListResponse, JSONRPCErrorError> {
        let ThreadBackgroundTerminalsListParams {
            thread_id,
            scope,
            cursor,
            limit,
        } = params;
        let (thread_id, thread) = self.load_thread(&thread_id).await?;
        let mut owners = vec![(thread_id, thread)];
        if scope == ThreadBackgroundTerminalsScope::Subtree {
            for descendant in self.state_db_spawn_subtree_thread_ids(thread_id).await? {
                if descendant == thread_id {
                    continue;
                }
                match self.thread_manager.get_thread(descendant).await {
                    Ok(thread) => owners.push((descendant, thread)),
                    // Unloaded descendants have no managed execution sessions. Do not
                    // resume them merely to project the live process inventory.
                    Err(CodexErr::ThreadNotFound(_)) => {}
                    Err(err) => return Err(internal_error(err.to_string())),
                }
            }
        }
        owners.sort_by_cached_key(|(owner, _)| owner.to_string());
        let mut terminals = Vec::new();
        for (owner, thread) in owners {
            terminals.extend(thread.list_background_terminals().await.into_iter().map(
                |terminal| ThreadBackgroundTerminal {
                    thread_id: owner.to_string(),
                    item_id: terminal.item_id,
                    process_id: terminal.process_id,
                    command: terminal.command,
                    cwd: terminal.cwd.into(),
                    os_pid: None,
                    cpu_percent: None,
                    rss_kb: None,
                },
            ));
        }
        let (data, next_cursor) = match scope {
            ThreadBackgroundTerminalsScope::Thread => {
                paginate_background_terminals(&terminals, cursor, limit)?
            }
            ThreadBackgroundTerminalsScope::Subtree => {
                paginate_subtree_background_terminals(&terminals, cursor, limit)?
            }
        };
        Ok(ThreadBackgroundTerminalsListResponse { data, next_cursor })
    }
}

pub(super) fn paginate_subtree_background_terminals(
    terminals: &[ThreadBackgroundTerminal],
    cursor: Option<String>,
    limit: Option<u32>,
) -> Result<(Vec<ThreadBackgroundTerminal>, Option<String>), JSONRPCErrorError> {
    let start = match cursor {
        Some(cursor) => {
            let (owner, process_id): (ThreadId, i32) = serde_json::from_str(&cursor)
                .map_err(|err| invalid_request(format!("invalid subtree cursor: {err}")))?;
            let owner = owner.to_string();
            terminals.partition_point(|terminal| {
                background_terminal_key(terminal) <= (owner.as_str(), process_id)
            })
        }
        None => 0,
    };
    let effective_limit = limit.unwrap_or(terminals.len() as u32).max(1) as usize;
    let end = start.saturating_add(effective_limit).min(terminals.len());
    let next_cursor = (end < terminals.len()).then(|| {
        serde_json::to_string(&background_terminal_key(&terminals[end - 1]))
            .expect("managed execution session cursor serializes")
    });
    Ok((terminals[start..end].to_vec(), next_cursor))
}

fn background_terminal_key(terminal: &ThreadBackgroundTerminal) -> (&str, i32) {
    (
        terminal.thread_id.as_str(),
        terminal
            .process_id
            .parse::<i32>()
            .expect("managed execution session IDs are integers"),
    )
}
