# Fork teleology

A telos is a purpose that a fork change serves and upstream does not; resolving a conflict the way upstream would can silently undo it. Each entry states one telos in six fields:

- Telos: why it exists.
- Holds: the invariant, stated so that a test can check it.
- Owners: the files and types that implement it.
- Tripwire: the test or check that fails when the invariant breaks. "Noisy" names broad snapshots that also change with unrelated upstream work and so tend to be accepted in bulk; "none" means no check fails.
- Pressure: how upstream threatens it. Pull request numbers name upstream changes between `rust-v0.160.0` and `rust-v0.162.0`.
- Evidence: fork commits.

Code marks a telos with `TELOS(slug): <one line>` in a doc or line comment where a merger decides: a seam in an upstream file, a fork-owned item whose bypass defeats the telos, or a fork default. Every tag names an entry here. Model-visible prompt templates never carry a tag, because it would reach the model. Text the fork deleted from the model context and capabilities it removed leave no code to tag; their entries name the absence check that guards them.

During a merge, look up each conflicted path in the index at the end and resolve the conflict so that every entry listed for it still holds. If upstream's change cannot coexist with an entry, ask the fork's maintainer instead of choosing.

## Model-context economy

### terse-exec-envelope

- Telos: a native execution result costs the model only what it acts on.
- Holds: rendered with `ExecutionOutputMetadata::Hidden`, an `exec_command` or `write_stdin` result is its output alone after exit 0, `Completed` after exit 0 without output, `Exit N` then output after another exit, and `Running (session S)` then output while running; it carries no wall time, chunk ID or token count, keeps truncation markers, and falls back to full JSON only for a contradictory status. MCP results carry no `Wall time` header.
- Owners: `codex-rs/protocol/src/native_execution_result.rs` (`NativeExecutionResult::render`, `render_batch`), `codex-rs/core/src/tools/context.rs` (`ExecCommandToolOutput::response_text`, `McpToolOutput`), `codex-rs/code-mode-runtime/src/runtime/native_result.rs`, `codex-rs/core/src/tools/code_mode/output.rs`.
- Tripwire: `native_result_presentation_follows_each_promise_and_current_values` (`codex-rs/code-mode-runtime/src/runtime/module_loader_tests.rs`); `exec_command_tool_output_*` in `codex-rs/core/src/tools/context_tests.rs`; `code_mode_can_return_exec_command_output` (`codex-rs/core/tests/suite/code_mode.rs`); the result parsers in `codex-rs/core/tests/suite/unified_exec.rs` and `approvals.rs`.
- Pressure: upstream tests assume the verbose envelope; 0.162 edits `unified_exec.rs` (3 commits), `approvals.rs` (4) and `code_mode.rs` (5) suites, including #50402 (`aggregated_output`).
- Evidence: 9ad927f511, 799b7a0998, 046e18a297.

### recoverable-output

- Telos: batching must not erase a middle command or require re-running work to recover emitted text.
- Holds: each nonempty text emission is a section; native execution arrays and `sh` preserve indexed result or coalesced-empty sections. An emission cap covers its entire batch, and a shared observation budget is fairly apportioned across all sections after model-policy reduction. Every reduced requested cap is disclosed. Truncated sections keep UTF-8-safe heads and tails; tiny budgets keep an omission marker rather than erasing section identity. Media, bounded notices and minimum markers are outside the text payload allowance. A finite allowance derived from the finished output prevents history recording or replay from truncating it again. Media preparation increases an explicit allowance only by the text tokens it introduces, so a failed image retains its diagnostic without relaxing the original output cap. Only truncated emitted text is retained, never unprinted variables or bytes a nested tool already discarded. Recovery uses private leased Codex-runtime artifacts, at most 16 MiB each and 64 MiB / 32 artifacts per thread, FIFO-evicted and removed on thread-service shutdown; abandoned artifacts require the owned lease marker before reaping. Storage failure is disclosed. Host-local paths do not grant a remote executor access.
- Owners: `codex-rs/core/src/tools/code_mode/output_sections.rs`, `output.rs` and `response_adapter.rs`; `codex-rs/core/src/session/prepared_tool_budget.rs`, with its call in `session/inject.rs`; `codex-rs/code-mode-runtime/src/runtime/native_result.rs` and `sh.rs`; `codex-rs/code-mode-protocol/src/response.rs` and `grpc/codex.code_mode.v1.proto`; `codex-rs/protocol/src/native_execution_result.rs`.
- Tripwire: `truncated_sections_share_the_budget_and_recover_only_truncated_text` (`codex-rs/core/src/tools/code_mode/mod.rs`), `completed_timing_preserves_content_and_distinguishes_zero_from_unavailable` (`output_tests.rs`), `code_mode_replaces_malformed_image`, `code_mode_exec_outer_limit_truncates_emitted_output` and the nested-limit cases (`codex-rs/core/tests/suite/code_mode.rs`); native presentation and host gRPC/stdio round trips cover the grouped wire shape.
- Pressure: flattening batches before budgeting, prefix-only history truncation and lost transport metadata silently undo recoverability; temporary files without custody leak data or delete another session's live output.
- Evidence: owner implementation decision, 2026-10-18; 2b9c80d3aa (prepared-media allowance).

### mcp-roster

- Telos: retain a broad tool catalogue without charging every conversation for every full schema. The short eager roster's per-tool context cost is intentional.
- Holds: in Code Mode Only, the roster gives the exact JavaScript member and one short purpose sentence for every nested tool whose contract is deferred. Tools declared in full are not repeated in the roster. The roster is the `<nested_tools>` world-state section: a window states it once in its opening context, a catalogue change amends it by name, and it is never part of the `exec` declaration, so no roster change moves a declaration on any protocol. It is context, not a managed instruction: compaction reads it as retained, keeping its request's cached prefix, and the next window restates it. A prediction fork that preserves inherited declarations leaves the inherited roster unamended. Bootstrap `exec_command`, `apply_patch`, and `tool_search` keep eager porcelain contracts; MCP contracts, remaining builtin contracts, and MCP server instructions arrive through `describe_tool`. Non-MCP client-supplied dynamic tools retain their configured exposure policy. Provider-sensitive reference spelling never changes a JavaScript member or native wire alias. Disclosure never removes execution capability.
- Owners: `codex-rs/code-mode-protocol/src/roster.rs` (`UndeclaredTool`, `roster`, `render_roster`, `render_roster_amendment`), `codex-rs/core/src/context/world_state/nested_tools.rs` (`NestedToolsState`, `WorldState::add_nested_tools`), `codex-rs/core/src/session/world_state.rs` (`build_world_state_for_step`), `codex-rs/core/src/tools/router/nested_tools.rs`, `codex-rs/code-mode-protocol/src/description.rs` (`build_exec_tool_description`, which declares eager contracts only), `codex-rs/core/src/tools/spec_plan.rs` (`register_code_mode_executors`), `codex-rs/core/src/tools/code_mode/disclosure.rs`, `codex-rs/core/src/tools/code_mode/execute_spec.rs`.
- Tripwire: absence of declarations: `roster_retains_every_callable_and_purpose_in_each_dialect` (`codex-rs/code-mode-protocol/src/roster_tests.rs`), `code_mode_uses_the_first_normalized_tool_identity` (`codex-rs/core/src/tools/spec_plan_tests.rs`), `code_mode_excludes_mcp_servers_using_their_configured_identity` (`codex-rs/core/tests/suite/code_mode.rs`), connector cases in `codex-rs/core/tests/suite/app_tool_exposure.rs`. Amendments and a fixed declaration: `changed_sections_and_entries_append_without_repeating_the_catalog` (`codex-rs/core/tests/suite/scenarios_exec_tools.rs`), `exec_description_ignores_the_undeclared_catalog` (`codex-rs/code-mode-protocol/src/description.rs`). Inherited roster: `prediction_fork_keeps_inherited_tools` (`codex-rs/core/tests/suite/scenarios_incremental_tools.rs`). Description token budget and server-instruction routing: none.
- Pressure: 0.162 rewrites the same builders: #51209 (ranked `tool_search` in Code Mode), #50562 (discovery guidance always present), #50536 (shared MCP types always present in Code Mode Only), #50687 (strict third-party deferral) edit `description.rs`, `spec_plan.rs` or `execute_spec.rs`.
- Evidence: 9b8fa789f6; owner clarification, 2026-10-13. Roster moved out of the declaration by owner-delegated decision, 2026-11-25: across seven native sessions, 9 of the 10 requests that followed a tool-declaration change were uncached, against 12 of 7,998 otherwise.

### append-only-tool-declarations

- Telos: a tool catalog change appends context instead of invalidating the conversation's cached prefix.
- Holds: `incremental_tools` defaults on; new Responses Lite context windows retain their initial declarations and append changed declarations and removal notices. Moving a callable member from an inline declaration to the roster is not a removal, including on resume from a pre-roster snapshot. The complete MCP name-and-purpose roster remains eager, including strict third-party mode, and changes by `<nested_tools>` amendment, never by declaration (`mcp-roster`); full MCP contracts remain on demand. Unsupported protocols retain their existing declaration path. Prediction/sample forks preserve inherited declarations unless `prediction_preserve_parent_tools = false`; a fork whose parent declared MCP tools also receives an MCP unavailability notice, which confers no MCP execution authority. An explicit `incremental_tools = false` selects that path for new windows; existing windows keep their established mode until replacement. Where declarations cannot be appended, cache churn from genuine catalogue or signature changes is accepted; this does not permit lookup-driven declaration changes.
- Owners: `codex-rs/features/src/lib.rs` (`Feature::IncrementalTools`), `codex-rs/core/src/session/step_context.rs` (`incremental_tools_enabled`), `codex-rs/core/src/session/world_state.rs` (`current_window_uses_incremental_tools`, `build_world_state_for_step`).
- Tripwire: `incremental_tools_append_changed_catalog_without_rewriting_history`, `tool_removal_appends_a_standalone_notice_without_rewriting_history` (`codex-rs/core/tests/suite/scenarios_incremental_tools.rs`), `strict_3p_mcp_refresh_preserves_cache_while_exec_tracks_available_tools` (`codex-rs/core/tests/suite/scenarios_strict_3p_cache.rs`), resume and opt-out cases in `codex-rs/core/tests/suite/scenarios_incremental_tools_resume.rs`.
- Pressure: upstream defaults this feature off; strict-third-party cache tests assume that no roster declaration changes, rather than permitting append-only updates.
- Evidence: release integration decision, 2026-10-11; owner clarification, 2026-10-13; prediction preservation made default-on by owner decision, 2026-11-03.

### compact-contracts

- Telos: discovery shows a compact summary; full contracts and output schemas cost tokens only when requested.
- Holds: `search_tools` and `ALL_TOOLS` give a name and one purpose sentence of at most 160 characters. `describe_tool` accepts canonical enabled-tool names and only unambiguous exact aliases such as `tools.NAME` or namespace dots; aliases never create callable bindings or promote tools. It derives deterministic porcelain from the executable schemas, placing invocation, parameters and return shape before extended prose: required and optional parameters, defaults, variants, constraints and return shape. References are expanded once per schema; input constraints are never silently budget-clipped. The obsolete Code Mode rendering-budget knob is removed; native MCP schema compaction is unchanged. `{detail: "output"}` discloses the complete declared result and `{detail: "schema"}` returns the retained machine schemas. MCP result fields are disclosed on demand, not repeated as a standing TypeScript preamble. For an unchanged catalogue, Code Mode discovery, contract lookup and first use never promote contracts into standing declarations or rewrite earlier context; their model-visible outputs append to history.
- Owners: `codex-rs/code-mode-protocol/src/discovery.rs` (`EnabledToolMetadata`, `compact_tool_summary`), `codex-rs/code-mode-protocol/src/contract.rs`, `codex-rs/code-mode-protocol/src/presentation.rs`, `codex-rs/code-mode-runtime/src/runtime/discovery.rs`, `codex-rs/core/src/tools/handlers/mcp.rs`, `codex-rs/core/src/tools/code_mode/execute_handler.rs`.
- Tripwire: `global_scope_contains_only_allowed_items` (`codex-rs/code-mode-runtime/src/service_tests.rs`) pins every discovery output; `contract_retains_shared_references_and_required_branches` (`codex-rs/code-mode-protocol/src/description.rs`); `mcp_code_mode_definitions_are_cached_lazily` (`codex-rs/core/src/tools/handlers/mcp.rs`) asserts the kept output schema. Request-level declaration stability across lookup and first use: unverified.
- Pressure: 0.162 #51209 returns TypeScript declarations from ranked `tool_search` and edits `execute_handler.rs`; #50687 changes MCP deferral in `handlers/mcp.rs`.
- Evidence: 3755ec0060, 87b76edda2, 8fc9bd12dc, 335de0047b; owner clarification, 2026-10-13.

### standing-context-trims

- Telos: the standing context carries nothing the model cannot use or already has.
- Holds (each an omission): `exec` documents `audio()` only for models with audio input and `generatedImage()` only when image generation is available; the three MCP resource tools are registered only when an initialized server advertises resources; `update_goal`'s description omits the completion and blocked audits that `ext/goal/templates/goals/continuation.md` carries; no model sees approved command prefixes; the permissions text omits writable roots and states its escalation paragraph once; the environment context folds rules repeated under every workspace root into `:workspace_roots` entries; mode texts state the current policy without retracting earlier ones; `exec_command` claims a PTY only for `tty`.
- Owners: `codex-rs/code-mode-protocol/src/description.rs` (`MediaHelpers`), `codex-rs/core/src/tools/spec_plan.rs`, `codex-rs/codex-mcp/src/connection_manager.rs` (`McpConnectionSet::offers_resources`), `codex-rs/codex-mcp/src/binding.rs`, `codex-rs/ext/goal/src/spec.rs`, `codex-rs/core/src/session/world_state.rs`, `codex-rs/prompts/src/permissions_instructions.rs`, `codex-rs/prompts/templates/permissions/approval_policy/on_request.md`, `codex-rs/core/src/context/environment_context/workspace_root_folding.rs`, `codex-rs/collaboration-mode-templates/templates/default.md`, `codex-rs/core/src/tools/handlers/shell_spec.rs`, `codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs`.
- Tripwire: approved prefixes: `saved_prefix_only_bypasses_guardian_for_general_models` (`codex-rs/core/tests/suite/cyber_exec_policy.rs`). The rest is noisy: `codex-rs/core/tests/suite/snapshots/all__suite__scenarios__*.snap` and `codex-rs/core/src/context/world_state/snapshots/`; no absence or budget assertion.
- Pressure: 0.162 #50546 always registers the MCP resource tools in Code Mode Only so that tool definitions stay stable across server changes, reversing the resource gate; #50536 always includes the shared MCP type preamble; #50540, #50447 and #49560 edit `world_state.rs`; #50962 and #50741 edit `shell_spec.rs` and `exec_command.rs`.
- Evidence: e7942f28c6, 1ed0c355cd.

### claude-native-voice

- Telos: native Claude is addressed as itself inside Codex-1337, not as Claude Code or as a GPT.
- Holds: the second `system` block of every native request is `subscription_harness`, which names the model and the harness; Claude catalog entries carry `ROOT_AGENT_ROLE` and `SUBAGENT_ROLE`, which use no GPT channel vocabulary, and list `request_user_input_async`; an agent message is one `<agent_message from to type>` element, as the role texts describe, and a host notification one `<tool_notification from>` element; their addresses and literal payload text are XML-escaped on this wire only; a text block that follows another in `system` or in a user message opens with a blank line, because the provider joins text blocks without a separator; a freeform tool's description names its `input` property and carries no grammar; a namespace's description opens its first tool's description, and later members carry neither it nor a reference to it.
- Owners: `codex-rs/codex-api/src/anthropic/request.rs` (`subscription_harness`), `codex-rs/codex-api/src/anthropic/history.rs` (`agent_message`, `separate`), `codex-rs/model-provider/src/claude_catalog.rs`, `codex-rs/core/src/client_anthropic_tools.rs` (`project_tools`, `custom`).
- Tripwire: role texts: `account_routes_do_not_leak_into_catalog_or_admit_openai_fallbacks` (`codex-rs/model-provider/src/claude_catalog_tests.rs`). Envelopes and separators: `positional_instruction_updates_do_not_rewrite_earlier_prefixes` (`codex-rs/codex-api/src/anthropic/history_tests.rs`). Harness block and namespace placement: none.
- Pressure: none in 0.162; every owner is fork-owned.
- Evidence: 9f19e7809e, 4d5c1e66f3.

### human-only-facts

- Telos: facts shown to the human cost no model tokens and leave the cached prefix unchanged.
- Holds: Code Mode ownership, MCP edit receipts, work status, the Preamble census, token bills, reasoning-token Thoughts and prompt-cache estimates travel as events or `TurnItem`s, never as `ResponseItem`s; `take_file_changes` strips receipts from the model-visible MCP result. Cosmetic status is registered in every session; suppressed roles accept a no-op. Malformed updates return warnings and overlong text is normalized and truncated, never allowed to abort a Code Mode cell.
- Owners: `codex-rs/protocol/src/code_mode.rs` (`CodeModeOwner`, `CodeModeOwned`), `codex-rs/protocol/src/mcp_file_changes.rs` (`FileChangeReceipt`, `take_file_changes`), `codex-rs/core/src/mcp_tool_call.rs`, `codex-rs/core/src/tools/handlers/work_status.rs`, `codex-rs/protocol/src/preamble.rs` (`PreambleItem`), `codex-rs/protocol/src/items.rs` (`TurnItem`), `codex-rs/rollout/src/policy.rs`, `codex-rs/core/src/session/response_thought.rs`.
- Tripwire: receipts: `mcp_receipt_survives_event_bounds_without_entering_model_result` (`codex-rs/core/src/mcp_tool_call_tests.rs`), `receipt_capture_is_private_and_rejection_does_not_change_tool_failure` (`codex-rs/protocol/src/mcp_file_changes_tests.rs`). Status: `work_status_replaces_activity_without_entering_history` (`codex-rs/core/tests/suite/work_status.rs`); other facts rely on the type split.
- Pressure: 0.162 #50458 (truncated MCP results in paginated history) and #50435 (tool definitions in rollout history) edit `mcp_tool_call.rs`, `rollout/src/policy.rs` and `protocol/src/models.rs`.
- Evidence: 67c459e3df, 900acaab71, dba3834511, 40394ab352.

### bare-model-slugs

- Telos: agent model selectors cost and read as bare slugs.
- Holds: `spawn_agent` advertises every selectable, backend-supported model by its bare slug unless that slug occurs in more than one provider's multi-agent catalog, hidden entries included, and then as `provider/slug`. Catalog budgets may shorten prose, never omit selectors or capabilities.
- Owners: `codex-rs/core/src/agent/model_catalog.rs` (`advertised_agent_models`).
- Tripwire: `fresh_child_routes_model_and_provider_but_history_forks_stay_locked` (`codex-rs/core/src/agent/model_catalog_tests.rs`).
- Pressure: 0.162 #49560 adds an opt-in model catalog to the multi-agent context through `spec_plan.rs` and `world_state.rs`.
- Evidence: 8b99bb1b59.

### turn-economy

- Telos: the model spends a round trip only when there is news.
- Holds: Code Mode `exec` and `wait` return only when the cell finishes or yields, user input or released mail arrives, or `[features.code_mode] max_yield_time_ms` (default 3600000) expires, and ignore the model's `yield_time_ms`; in Code Mode Only, `exec_command` waits for exit unless `background` is set, and empty `write_stdin` normally waits at least 60 s, returning sooner on completion or interjection; terminal polling permits a one-hour window while nonempty input remains responsive. Direct empty polls yield to steering without discarding drained bytes or terminating the process; nested polls remain governed by their cell's continuation, not its originating step's latched signal; completed-process observations retain a bounded terminal receipt, never stdin authority, within the owning session and process-manager lifetime. Repeated empty polls may replay the terminal tail without repeating hooks; evicted IDs are never reused in that manager; ordinary family mail wakes idle recipients and batches for one fixed window while at least two subagents are active anywhere in the family; `wait_agent` observes released mail without adding another hold; Code Mode exposes `clock.sleep`.
- Owners: `codex-rs/core/src/session/input_queue/interjection.rs` (`Interjection`), `codex-rs/core/src/tools/code_mode/wait_handler.rs`, `codex-rs/core/src/tools/code_mode/execute_handler.rs`, `codex-rs/core/src/session/input_queue/mailbox_batch.rs`, `codex-rs/core/src/unified_exec/initial_wait.rs` (`InitialWait`), `codex-rs/core/src/unified_exec/completion_receipts.rs`, `codex-rs/core/src/unified_exec/output_collection.rs`, `codex-rs/core/src/tools/handlers/unified_exec/exec_command/cell.rs`, `codex-rs/core/src/unified_exec/mod.rs` (`MIN_EMPTY_YIELD_TIME_MS`), `codex-rs/core/src/tools/spec_plan.rs`.
- Tripwire: `interjections_yield_exec_and_wait_without_stopping_the_cell` and `wait_agent_wake_preserves_mail_and_steers` (`codex-rs/core/tests/suite/pending_input.rs`), `code_mode_can_return_exec_command_output` (`codex-rs/core/tests/suite/code_mode.rs`), `sleep_tool_uses_configured_time_provider` (`codex-rs/core/tests/suite/current_time_reminder.rs`). Direct-poll steering and receipt replay: `write_stdin_returns_exit_metadata_and_clears_session` (`codex-rs/core/tests/suite/unified_exec.rs`). The 60 s floor is a constructor clamp, not a wall-clock test.
- Pressure: 0.162 #51812 enables `instant_interrupt` by default, which decides whether an `Interjection` cuts off streaming, and edits `pending_input.rs`; #51209 edits `code_mode/mod.rs` and `execute_handler.rs`; #51332 edits `multi_agents_v2/wait.rs`; #50962 and #50741 edit `exec_command.rs` and `write_stdin.rs`.
- Evidence: 25226df915, 2d9f646044, 9050de4515, cc4701d2fe.

### local-coprocessor

- Telos: local computation and bounded file transfers need no shell quoting or extra model turn.
- Holds: raw Python and Bash source executes through the selected executor with ordinary execution policy, approval and cancellation; script execution does not acquire shell startup, snapshot or command-prefix approval semantics. Code Mode filesystem helpers use the captured step's environment and granted permissions, never the host's ambient filesystem authority; each transfer is bounded to 1 MiB. The V8 runtime provides `TextEncoder`, `TextDecoder`, `atob` and `btoa` with streaming Unicode and Latin-1 Base64 semantics, not a browser or Node environment. Invalid Base64 raises an `Error` named `InvalidCharacterError`; there is no `DOMException` global.
- Owners: `codex-rs/core/src/tools/script_exec.rs` (`ExecutionMode`), `codex-rs/core/src/tools/handlers/script_exec/`, `codex-rs/core/src/exec_policy/script_exec.rs`, `codex-rs/core/src/tools/handlers/code_mode_fs.rs`, `codex-rs/code-mode-runtime/src/runtime/filesystem.rs`, `codex-rs/code-mode-runtime/src/runtime/encoding.rs` and `encoding.js`; seams in `tools/spec_plan.rs`, `tools/handlers/unified_exec/exec_command.rs`, `tools/runtimes/unified_exec.rs` and `unified_exec/process_manager.rs` under `codex-rs/core/src/`.
- Tripwire: `raw_script_execution_contract` (`codex-rs/core/tests/suite/raw_script_exec.rs`), `filesystem_authority_and_transfer_bounds` (`codex-rs/core/tests/suite/code_mode_pipeline.rs`); `global_scope_contains_only_allowed_items` and `decoder_state_follows_its_javascript_owner` (`codex-rs/code-mode-runtime/src/service_tests.rs`) cover encoding semantics and lifetime.
- Pressure: changes to shell invocation, snapshot preparation, executor selection or turn permission grants must preserve script-versus-shell authority; Code Mode description overrides must not hide the filesystem helpers.
- Evidence: 0dc126edd9, 0a51b60ef1.

### agent-bash

- Telos: agent command syntax does not depend on the human's choice of interactive shell.
- Holds: supported Linux agent execution selects Bash for both session defaults and executor-reported shell context, independently of the human's login shell. Missing Bash fails execution rather than silently interpreting the source with another shell. Raw `bash` is Bash; `exec_command` and Code Mode's `sh` helper use the agent default unless an explicit override or an external executor's reported shell applies. Inherited environment, PATH, permission policy and cancellation retain their existing owners. Human-only TUI `!!` commands remain on the separate local-shell path.
- Owners: `codex-rs/shell-command/src/agent_shell.rs`, with seams in `codex-rs/core/src/session/session.rs` and `codex-rs/exec-server-protocol/src/protocol.rs`; model-facing shell contract in `codex-rs/core/src/tools/handlers/shell_spec.rs`.
- Tripwire: `exec_command_uses_installed_environment_shell_policy_with_explicit_overrides` (`codex-rs/core/tests/suite/unified_exec.rs`) couples executed Bash syntax, reported shell and selected-environment policy. Human-local authority remains covered by [client-only-shell](#client-only-shell).
- Pressure: upstream default-shell detection follows the human's login shell; reusing it for agents silently changes syntax, startup behavior and glob expansion.
- Evidence: owner doctrine decision, 2026-10-18.

### response-order-fences

- Telos: dependent execution stages fit in one model response without racing or running after a failed prerequisite.
- Holds: calls enter their response batch in stream order; a fence admits the next stage only after every preceding call has completed successfully. A Code Mode cell owns its verdict: caught tool errors and handled nonzero process exits are data, not automatic stage failures. Uncaught exceptions and terminal `fail(message)` fail the cell; `fail` cannot be caught or undone by `exit`. Cancellation, dispatcher loss, fatal host failures, dropped or unawaited calls, any yielded cell and background work still fail closed. Direct top-level tools retain their own failure verdicts. Admission waits occur outside execution locks, and Code Mode coordinators hold no lock while their nested tools execute. A new model response starts a new batch.
- Owners: `codex-rs/core/src/tools/fence.rs`, `codex-rs/core/src/tools/parallel.rs`, `codex-rs/core/src/stream_events_utils.rs`, `codex-rs/core/src/session/turn.rs`, `codex-rs/core/src/tools/code_mode/delegate.rs`, `codex-rs/core/src/tools/spec_plan.rs`, `codex-rs/code-mode-protocol/src/description.rs`.
- Tripwire: `response_stage_pipeline` (`codex-rs/core/tests/suite/code_mode_pipeline.rs`) covers successful and failed stages; scheduler and cancellation cases live beside the fence implementation.
- Pressure: stream dispatch, parallel-tool locking and Code Mode worker boundaries can lose ordering or confuse handled data with the cell's terminal outcome; model description overrides must retain the fence contract.
- Evidence: 7e44726e4e; owner cell-outcome doctrine decision, 2026-10-18.

### retained-goal-rules

- Telos: continuing a goal pays for its full rules once per retained context, rather than once per continuation.
- Holds: the goal extension contributes full continuation rules as an owned world-state section and emits short subsequent reminders. A saved section snapshot cannot suppress rules absent from retained history; only a complete, kind-aligned fragment with matching provenance proves presence, never unannotated matching user text.
- Owners: `codex-rs/ext/goal/src/world_state.rs`, `codex-rs/ext/goal/src/extension.rs`, `codex-rs/ext/goal/src/runtime.rs`, `codex-rs/core/src/context/world_state/mod.rs`; prompt text remains in `codex-rs/ext/goal/templates/goals/`.
- Tripwire: `missing_retained_fragment_is_rendered_again` (`codex-rs/core/src/context/world_state/world_state_tests.rs`); existing goal continuation cases cover the emitted reminders.
- Pressure: compaction, history truncation and changes to world-state rendering or content-kind alignment must not drop rules or restore repeated full instructions.
- Evidence: a7c3becfcf.

## Prompt-cache stability

### instruction-amendments

- Telos: ordinary instruction changes append amendments rather than rewrite the Preamble.
- Holds: managed instructions retain their original authority and use literal unified diffs. The harness keeps canonical full values and requires complete, kind-aligned retained ancestry before suppressing a reinjection. Compaction receives a disposable view with current full instructions substituted for their older versions and amendments; changed and inserted items receive fresh request identities, while conversation evidence and opaque provider checkpoints remain intact. Only successful compaction replaces live history and starts a new window. Model/capability changes, local tip repairs and unsupported incremental tool protocols retain their existing behavior; rollback has no special optimization.
- Owners: `codex-rs/core/src/context/world_state/instruction_diff.rs`, `codex-rs/core/src/context/world_state/instruction_projection.rs`, `codex-rs/core/src/context_manager/instruction_projection.rs`, `codex-rs/core/src/context/base_instructions.rs`.
- Tripwire: `render_diff_restores_the_typed_section_snapshot`, `catalog_collaboration_messages_refresh_without_mode_or_model_change`, `compaction_preserves_updated_summary`; request identities: `response_item_ids_are_sent_for_all_remote_v2_compaction_requests` (`codex-rs/core/tests/suite/client.rs`).
- Pressure: instruction providers must expose a canonical full value without emission side effects; fragment provenance and authority must survive replay. Skills opt in through `WorldStateSectionContribution::with_instruction_amendments`: added/changed/removed summaries use unaliased paths and share the harness's retention and compaction machinery. Bounded catalogs relist rather than amend incomplete membership. Legacy revocations without a canonical predecessor retain explicit notices outside materialization. `production_turn_keeps_host_aliases_and_amends_catalog_changes` covers amendment materialization and cold replay.

### no-vacuous-injections

- Telos: nothing enters the model's context that says nothing.
- Holds: an instruction amendment exists only when an instruction's authority or text changes: a revision differing only in kind or provenance is adopted silently, and agreeing texts yield no diff. A world-state fragment with a blank body is dropped when nothing precedes it to revoke, so a blank full instruction is no instruction; against known or unknown history it may still revoke. A blank hook additional context reaches neither the model nor the transcript.
- Owners: `codex-rs/core/src/context/world_state/instruction_diff.rs` (`Entry::render`, `render_eloquent`, `diff`), `codex-rs/hooks/src/events/common.rs` (`append_additional_context`).
- Tripwire: `render_diff_restores_the_typed_section_snapshot`. Blank hook contexts: none.
- Pressure: persisted revisions predating content provenance carry default metadata, so an exact comparison amended unchanged text with hunkless diffs after an upgrade; upstream renders permissions instructions even when every catalog message is blank, as in guardian reviews; hook outputs pass `additionalContext` through unfiltered.
- Evidence: 5dbeefa6a3.

### code-mode-cell-outcomes

- Telos: a code-mode cell's reported terminal status is its runtime outcome.
- Holds: the fork's `cell_finished` callback is required on `CodeModeSessionDelegate` and `SessionRuntimeDelegate`, so every delegate, wrapper included, forwards or explicitly ignores the committed outcome; `cell_closed` without a prior outcome reports interruption.
- Owners: `codex-rs/code-mode-protocol/src/session.rs` (`CodeModeSessionDelegate`), `codex-rs/code-mode-runtime/src/session_runtime/types.rs` (`SessionRuntimeDelegate`), `codex-rs/code-mode/src/remote_session/delegate.rs` (`TracedCellDelegate`), `codex-rs/core/src/tools/code_mode/delegate.rs` (`CodeModeCellDelegate`).
- Tripwire: `remote_session_persists_values_forwards_delegates_and_controls_cells` (`codex-rs/code-mode-host/tests/stdio.rs`).
- Pressure: 0.165 wraps stdio-host callbacks in `TracedCellDelegate`, which forwarded only upstream methods, so the then-defaulted fork callback reported every cell interrupted. Each new upstream delegate must implement `cell_finished`.
- Evidence: 3b3b175874.

### reminders-not-system

- Telos: an instruction update never rewrites an earlier cached prefix.
- Holds: besides the two subscription blocks, a native request's `system` holds only the instructions that open the window: those that precede the first message or directly follow a leading compaction checkpoint, so every window has the same layout. Each later system or developer item becomes, at its original position, a user message of `<system-reminder>` text blocks.
- Owners: `codex-rs/codex-api/src/anthropic/history.rs` (`Encoder`).
- Tripwire: `positional_instruction_updates_do_not_rewrite_earlier_prefixes` (`codex-rs/codex-api/src/anthropic/history_tests.rs`).
- Pressure: 0.162 #51156 moves base instructions from the `instructions` field into a leading `developer` input message for Responses requests; the native encoder reads `prompt.instructions`.
- Evidence: 15bb22a28e, e2e1f4a101.

### catalog-at-model-boundaries

- Telos: a changing MCP catalog never alters the declarations of a request in flight.
- Holds: an ordinary MCP list-changed notification refreshes catalogs and invalidates cached bindings only at the next model boundary; a call whose advertised declaration differs from the live one is rejected, except that a listing whose annotations a cache or dormant server withheld (`AnnotationSource::Withheld`) dispatches with the live annotations; an annotation change between two live listings still rejects.
- Owners: `codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs` (`prepare_call_for_tool`), `codex-rs/codex-mcp/src/client_tool_catalog.rs`, `codex-rs/codex-mcp/src/runtime.rs`, `codex-rs/codex-mcp/src/tools.rs` (`AnnotationSource`), `codex-rs/codex-mcp/src/tool_catalog_cache.rs`, `codex-rs/core/src/mcp_tool_call.rs`.
- Tripwire: `ordinary_notifications_invalidate_cached_bindings_at_the_next_boundary` (`codex-rs/codex-mcp/src/runtime/catalog_refresh_tests.rs`), `ordinary_catalog_refresh_coalesces_and_preserves_failed_and_racing_notifications` (`codex-rs/codex-mcp/src/connection_manager/tool_catalog_refresh_tests.rs`), which covers annotation-only edits. Withheld-annotation dispatch: none.
- Pressure: 0.162 incremental tool catalog updates (#50464, #50540, #51119, #51188, #51202, #51480) change the declared catalog between requests within a context window; #51215 edits `tool_catalog.rs`.
- Evidence: e2267aec40, d1efade548, 32ac1fd013.

### stable-native-identities

- Telos: catalogue changes, retirements and resumes leave a native conversation's bytes, and so its cache prefix, unchanged.
- Holds: a native tool's wire alias depends only on its identity (`AnthropicToolIdentity::wire_alias`), never on catalogue order or membership; history naming a retired tool replays identically without re-declaring it; a failed tool output resumes with `success == Some(false)` through the persisted `tool_failed` harness mark.
- Owners: `codex-rs/codex-api/src/anthropic/tool_alias.rs`, `codex-rs/core/src/client_anthropic_tools.rs`, `codex-rs/codex-api/src/anthropic/history.rs`, `codex-rs/history/src/tool_failure.rs`.
- Tripwire: `aliases_preserve_dispatch_identity_across_collisions_and_catalogue_order` (`codex-rs/core/src/client_anthropic_tools_tests.rs`), `signed_history_and_parallel_tool_results_preserve_cache_prefix` (`codex-rs/codex-api/src/anthropic/history_tests.rs`), `resumed_native_history_replays_tool_failure` (`codex-rs/core/src/session/rollout_reconstruction_tests.rs`), `native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm` (`codex-rs/core/src/client_anthropic_tests.rs`).
- Pressure: 0.162 #50435 persists additional tool definitions in rollout history and #49401 keeps live tool-call metadata across request windows, both in `protocol/src/models.rs`.
- Evidence: ea7dfec93c, 60dd4c7118, ef357510f1, 846fd2257f.

### cache-ttl-and-warming

- Telos: cache warmth follows observed usage; hour-lived Claude caches are chosen where they pay and kept warm while the human is present.
- Holds: this deployment uses GPT-6-family or newer OpenAI models. Their conversation requests establish a 30-minute passive Warm estimate only after reporting cache reads or writes; API and subscription routes use the same window. Other Responses providers do not inherit that policy. Expiry runs conservatively from dispatch; prefix invalidation and absent cache usage clear the estimate. A restart creates no lease from thread metadata. This does not enable OpenAI `/warm` or send background requests.
- Holds: a native request's cache TTL is that of the first `anthropic_prompt_cache.rules` entry matching its model prefix and effective effort, else `ttl` (default one hour); `/warm` toggles warming only for hour-lived root Claude caches; a hold lasts 8 h from the last human message or toggle; the sealed request replays 3 min before expiry; any lapse turns warming off; warming never heats a cold cache.
- Owners: `codex-rs/core/src/client_prompt_cache.rs`, `codex-rs/config/src/anthropic_prompt_cache.rs` (`AnthropicPromptCache::resolve`), `codex-rs/codex-api/src/anthropic/cache.rs`, `codex-rs/codex-api/src/anthropic/warm.rs`, `codex-rs/app-server/src/prompt_cache.rs`, `codex-rs/app-server/src/request_processors/thread_warming.rs`, `codex-rs/app-server-protocol/src/protocol/v2/prompt_cache.rs`.
- Tripwire: `ordered_rules_filter_effective_effort_and_validate_selectors` (`codex-rs/config/src/anthropic_prompt_cache_tests.rs`), `observed_cache_usage_controls_warmth_across_completion_attach_and_restart` (`codex-rs/app-server/tests/suite/v2/thread_warming.rs`), `warming_and_cold_glyphs_share_the_bottom_padding_row` (`codex-rs/tui/src/bottom_pane/chat_composer/prompt_cache_glyphs_tests.rs`), the TTL markers in `native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm`.
- Pressure: none in 0.162; every owner is fork-owned.
- Evidence: fc408f929c, 920a743485.

## Token visibility

### token-bills

- Telos: the human sees what consumes the context.
- Holds: every tool output, MCP result and Code Mode result disclosure shows its approximate model-visible tokens, after truncation for native Claude; each completed model response yields a human-only Thought with its reported reasoning tokens; bills render in the fixed rich-transcript gutter. A Thought occupies no separate transcript row: returned raw reasoning is available behind one initially closed `[cot]` on its response/group header, independent of reasoning-display configuration. Empty, summary-only, token-only and encrypted-only items have no `[cot]`. Grouping never duplicates a response’s reasoning across its tool calls; explicit disclosure preserves source copying and width reflow. Reasoning uses the shared orange-tinted content style, while header chrome remains on the terminal’s default background. Existing model reasoning/effort controls retain generation policy; no provider display gate is added. Preamble records a window's initial special material once, preserving typed prompt captures separately from client declaration representations, and survives reconnect and restart without a new census. Later changes appear as Injections; `/context` inspects retained special material without injecting it. Legacy rolling censuses remain readable under their own label. Census work never blocks response-usage recording or cancellation cleanup.
- Owners: `codex-rs/core/src/session/tool_output_tokens.rs`, `codex-rs/utils/output-truncation/src/token_count.rs`, `codex-rs/core/src/session/response_thought.rs`, `codex-rs/core/src/session/preamble.rs`, `codex-rs/app-server-protocol/src/protocol/v2/thought.rs`, `codex-rs/protocol/src/preamble.rs`, `codex-rs/tui/src/transcript_view/text_gutter.rs`, `codex-rs/tui/src/history_cell/preamble.rs`, `codex-rs/tui/src/history_cell/thought.rs`, `codex-rs/tui/src/transcript_view/feed.rs`, `codex-rs/tui/src/style/speaker_backgrounds.rs`.
- Tripwire: `tool_text_matches_reference_tokenizer_without_interpreting_control_markers` (`codex-rs/utils/output-truncation/src/token_count_tests.rs`), `thought_completion_is_self_contained_and_response_scoped` (`codex-rs/app-server-protocol/src/protocol/thread_history_projection_tests.rs`), `completed_thought_discloses_only_readable_reasoning` (`codex-rs/tui/src/transcript_view/tool_entries_tests.rs`), `steps_fold_under_one_header_with_their_figures` (`codex-rs/tui/src/transcript_view/feed_tests.rs`), `structured_content_remains_disclosable_in_direct_and_code_mode_results` (`codex-rs/tui/src/history_cell/mcp_tests.rs`), `tool_output_disclosure_shows_fixed_token_column` (`codex-rs/tui/src/transcript_view/layout_tests.rs`), `preamble_discloses_parts_largest_first` (`codex-rs/tui/src/history_cell/preamble_tests.rs`).
- Pressure: none in 0.162; every owner is fork-owned.
- Evidence: 5b6e3b5096, 0a2e39ff0f, 86f8dba3aa, cabee7f17e, 2213557e47, 40394ab352.

## Native Claude as a first-class provider

### messages-from-canonical-history

- Telos: native Claude reads Codex's canonical history directly, and no history it cannot encode wedges a thread.
- Holds: Messages requests are encoded item by item from canonical `ResponseItem` history, not through a Responses emulation; content recorded during an unresolved tool batch follows the batch's results; `salvage_anthropic_history` always yields an encodable history whose messages fit 30 of a request's 32 MB, shedding the oldest media first, and reports each `AnthropicRepair`; native compaction descends the `Rung` ladder (whole history, tail, recent user messages), and when even the last rung returns no checkpoint the thread resets to that material without a model call; each concession is a warning; a sampling history that the provider refuses, or that the encoder rejects and salvage would repair (an oversized request is an invalid prompt), compacts once per step and retries.
- Owners: `codex-rs/codex-api/src/anthropic/mod.rs` (`AnthropicRequest::from_body`), `codex-rs/codex-api/src/anthropic/history.rs`, `codex-rs/codex-api/src/anthropic/salvage.rs`, `codex-rs/core/src/compact_native.rs` (`Rung`), `codex-rs/core/src/compact_native_request.rs`.
- Tripwire: `content_recorded_during_a_tool_call_follows_its_batch` and `salvage_always_yields_an_encodable_history` (`codex-rs/codex-api/src/anthropic/history_tests.rs`), `unusable_completions_record_usage_and_name_their_rung_escalation` (`codex-rs/core/src/compact_native_request_tests.rs`).
- Pressure: 0.162 #51156 moves base instructions into input messages; #51117 installs full context in compaction replacement history (`compact.rs`, `compact_remote_v2.rs`); #51188 records base instructions in incremental tool history.
- Evidence: 65b19edd6c, d68f5c1e02; 6c09b92a17; LIBGRID_MASCOT wedged on 27 tool-result images (33.2 MB encoded) at 45% of its context window, 2026-10-30.

### subscription-boundary

- Telos: the fork uses a Claude subscription only as Anthropic's own client would, and never leaks its credentials.
- Holds: the first `system` block of every native request is exactly `SUBSCRIPTION_PREAMBLE`; a `ClaudeSubscription` provider sends credentials only to `https://api.anthropic.com/v1` and admits no custom credentials, headers or query parameters; a request captures its account before any await, and refresh and dispatch keep that account when the selection changes.
- Owners: `codex-rs/codex-api/src/anthropic/request.rs` (`SUBSCRIPTION_PREAMBLE`), `codex-rs/model-provider-info/src/lib.rs` (`ANTHROPIC_BASE_URL`, `ModelProviderInfo::validate`), `codex-rs/core/src/client_anthropic.rs` (`capture_anthropic_auth`, `with_anthropic_account`).
- Tripwire: account capture: `native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm` (`codex-rs/core/src/client_anthropic_tests.rs`). Preamble and endpoint: none; `scripts/native_anthropic/check.py` is manual, and its corpus predates the harness block.
- Pressure: 0.162 #50459 adds capability overrides for custom providers in `model-provider-info/src/lib.rs`.
- Evidence: 60e8198df4, b674b02d5a, 4157a2e4ea, ea648c2824. Owner decision, 2026-11-02: keep the standard Claude OAuth grant (8-hour access token, automatic refresh, renewal chain ending about four weeks after sign-in), not the 1-year inference-only setup-token grant, which would lose the account email and weekly-quota display.

### native-robustness

- Telos: a provider anomaly costs at most the affected call, and every charged token is recorded.
- Holds: a `tool_use` that names an undeclared tool or whose input does not fit its tool's envelope becomes `ResponseItem::RejectedToolCall` (`ToolCallRejection`): it is answered, not dispatched, and later blocks and usage survive; charged usage is recorded before an exhausted or refused response is rejected; subscription exhaustion maps to a usage-limit error; a refusal ends the turn with a terminal error naming the policy area Anthropic reports and commits no output a call has not already committed; completed text and thinking wait for the call that follows them or for the stop, so a recoverable interruption retries from recorded history, replaying nothing or continuing from answered calls; repeated `message_delta` events and unknown event types are accepted.
- Owners: `codex-rs/codex-api/src/anthropic/decoder.rs` (`AnthropicStopReason`), `codex-rs/codex-api/src/api_bridge.rs` (`AnthropicStream`), `codex-rs/protocol/src/models.rs` (`ToolCallRejection`), `codex-rs/core/src/stream_events_utils.rs`, `codex-rs/codex-api/src/anthropic/limits.rs` (`parse_usage_limit`), `codex-rs/core/src/session/turn.rs`, `codex-rs/core/src/compact_native_request.rs`.
- Tripwire: `undeclared_tool_preserves_stream_and_replays_as_a_paired_rejection`, `malformed_declared_tool_preserves_later_blocks_and_usage` and `interleaved_tools_signed_blocks_and_cumulative_usage` (`codex-rs/codex-api/src/anthropic/decoder_tests.rs`), `rejected_native_call_is_not_dispatched_and_keeps_its_result_on_resume` (`codex-rs/core/src/stream_events_utils_tests.rs`), `map_api_error_distinguishes_anthropic_subscription_exhaustion` (`codex-rs/codex-api/src/api_bridge_tests.rs`), `fragmented_utf8_crlf_sse_and_uncommitted_failures_replay` and `stream_failures_stay_typed_for_retry` (`codex-rs/codex-api/src/anthropic/stream_tests.rs`), the `Committed` profile of `native_request_pins_recovery_then_switches_with_exact_custom_tool_replay_and_cache_warm` (`codex-rs/core/src/client_anthropic_tests.rs`). Usage before rejection in the sampling path: none.
- Pressure: 0.162 #51241 and #51249 add a partial-answer message phase in `protocol/src/models.rs` and `stream_events_utils.rs`; `session/turn.rs` takes 16 upstream commits.
- Evidence: df7be75a07, f21241bd52, 6fa07b94ea, e12016f164, d54b293a72, 148c4ae73a; 2a3c2137eb; LIBGRID_MASCOT (2026-10-30) and thread 01a18836 (2026-10-29) ended on SSE failures after a completed thinking block or call; owner kept refusals terminal rather than opt into Anthropic's server-side fallback, 2026-10-30.

### adapter-not-transport

- Telos: provider semantics are a provider property, independent of the Responses wire format it may share.
- Holds: `ModelProviderAdapter`, not `WireApi`, selects provider semantics; context-window and compaction defaults follow the selected provider without being written into config; each provider ID has its own models cache file (`models_cache.json` for OpenAI, `models_cache_<id>.json` otherwise).
- Owners: `codex-rs/model-provider-info/src/lib.rs` (`ModelProviderAdapter`, `models_cache_file_name`), `codex-rs/core/src/config/models_cache.rs` (`Config::models_cache_path`), `codex-rs/core/src/config/mod.rs`, `codex-rs/core/src/session/context_window.rs`.
- Tripwire: `provider_context_defaults_follow_selection_without_baking_into_config` (`codex-rs/core/src/config/config_tests.rs`). Per-provider cache file: none.
- Pressure: 0.162 #50459 adds capability overrides for custom providers in `model-provider-info/src/lib.rs`; `core/src/config/mod.rs` takes 12 upstream commits.
- Evidence: 903087f7a4, f48c034081, 45892d83cc.

### provider-parity

- Telos: OpenAI and Anthropic are equally trusted, so either may serve any role.
- Holds: a privacy or data-locality rule permits or forbids both providers alike; a Claude conversation's approval review runs on an eligible saved ChatGPT account's auto-review model, and an individual review falls back to Claude for good after its first failed OpenAI attempt, within the shared deadline.
- Owners: `docs/fork.md` (Provider data), `codex-rs/core/src/guardian/reviewer_route.rs`.
- Tripwire: `claude_subscription_review_spawns_chatgpt_reviewer_with_saved_account` and `route_failure_quarantines_and_falls_back_only_with_remaining_time` (`codex-rs/core/src/guardian/reviewer_route_tests.rs`). The data rule: none.
- Pressure: 0.162 #51137 recovers Guardian reviews from parent checkpoints and #51223 edits `guardian/review_session.rs`.
- Evidence: ad002e7548, ff65685666.

## Tool authority

### managed-process-visibility

- Telos: the managed local server permits the owner's chosen host-process and user-service access without changing every execution request.
- Holds: production and staging units explicitly select inherited Linux PID/proc visibility; ordinary app-server startup still defaults to isolation. The trusted startup choice reaches only the local executor, never remote executor policy or a repository/model permission override. Other directly executed sandbox restrictions remain unchanged. Same-UID signaling and host-user-manager escape from tool containment are intentional operator authority; see `docs/fork.md` Local TUI releases.
- Owners: `codex-rs/app-server/src/local_exec.rs`, `codex-rs/sandboxing/src/linux_pid_namespace.rs`, `scripts/systemd/codex-app-server.service`, `scripts/systemd/codex-app-server-staging.service`.
- Tripwire: existing app-server startup/config and installer-unit checks cover argument propagation; host D-Bus behavior has a recorded read-only A/B witness, not a portable unit-test claim.
- Pressure: upstream sandbox defaults or daemon argument rebuilding must not erase the explicit managed-unit policy or silently apply it to standalone/remote executors.
- Evidence: 649efc61dc.

### startup-tool-ceiling

- Telos: an ephemeral utility model receives data without acquiring tool authority through configuration or provider changes.
- Holds: the captured startup tool policy is immutable and can only narrow inherited authority. `noTools` permits no direct or nested dispatch and starts no MCP contributors, including required servers. The frontend sends utility transcript/inference only after a receipt derived from the captured policy confirms the restriction; absent receipts fail closed. This nonpersisted startup restriction requires an ephemeral thread.
- Owners: `codex-rs/protocol/src/tool_policy.rs`, `codex-rs/core/src/tool_policy.rs`, `codex-rs/core/src/tools/registry.rs`, `codex-rs/tui/src/temporary_structured_request.rs`.
- Tripwire: `startup_allowlist_controls_advertising_and_execution` (`codex-rs/core/tests/suite/managed_threads_tests.rs`), `dispatch_uses_canonical_tool_names_for_lifecycle_contributors` (`codex-rs/core/src/tools/registry_tests.rs`), and the existing recap/older-server interactions.
- Pressure: startup, extension refresh and discovery changes must preserve the ceiling at advertising and execution, not merely hide schemas or maintain a feature denylist.
- Evidence: 7b8ddaa991.

### canonical-tool-identity

- Telos: formatting a tool name cannot broaden a hook, change attribution, or collide with another tool.
- Holds: namespace and local name remain one structured `ToolName` through dispatch, hooks and delayed execution receipts. `HookSelector` distinguishes tool identity from lifecycle-event values; unknown fields and selectors invalid for an event fail closed. Provider aliases and display strings are boundary projections, never policy identities; obsolete flattened matchers have no compatibility fallback.
- Owners: `codex-rs/protocol/src/tool_name.rs`, `codex-rs/protocol/src/hook_selector.rs`, `codex-rs/hooks/src/engine/selection.rs`, `codex-rs/core/src/tool_policy.rs`.
- Tripwire: existing hook selection/configuration interactions in `codex-rs/hooks/src/engine/mod_tests.rs` and `codex-rs/hooks/src/config_rules.rs`; original-tool attribution through execution is covered by the core hook integration cases.
- Pressure: upstream changes to tool naming, delayed process receipts or hook configuration must not reintroduce concatenated identities or turn an invalid selector into an unfiltered hook.
- Evidence: 46833d37fa, 731f798e13, 09cb78a61c.

## Server-owned coordination

### accepted-agent-mail

- Telos: ordinary mail acknowledged as accepted survives a server crash before model admission without becoming duplicate or revived work.
- Holds: the authoritative inbox commits a stable identity, attribution and replay digest before acceptance. Finite owned admission drains on shutdown and commits canonical history before acknowledging the row; an uncertain canonical write fences new inference until canonical recovery. Lookup checks retained raw lineage, past compaction but within explicit cutoffs, and never treats an old admission witness as current context membership. Revert acknowledges already-canonical pending rows before discarding their witnesses; genuinely unadmitted rows remain pending. Recovery requires no frontend, retains blocked mail and uses bounded in-memory backoff. The envelope is process crash with intact storage, not a preacceptance completion outbox.
- Owners: `codex-rs/core/src/agent/control/accepted_mail.rs`, `codex-rs/core/src/session/agent_mail.rs`, `codex-rs/state/src/runtime/agent_mail.rs`, `codex-rs/thread-store/src/live_thread/agent_mail.rs`, `codex-rs/thread-store/src/local/agent_mail.rs`, `codex-rs/app-server/src/request_processors/agent_mail.rs`.
- Tripwire: `record_inter_agent_communication_preserves_item_id_in_rollout_and_resume` (`codex-rs/core/src/session/tests.rs`), `revert_keeps_thread_id_and_hides_suffix_across_repeated_reverts` (`codex-rs/thread-store/src/local/revert_thread_tests.rs`), and the existing idle-mail/successor-process recovery interactions.
- Pressure: queued-input, task registration, compaction, rollback, lineage and shutdown changes must preserve acceptance versus admission and canonical uncertainty; neither latest-model-context reads nor best-effort channel sends replace the durable contract.
- Evidence: 53d5d972e3, c63e481a76, 2a47c1fcc3, 519609ba40, c882db1102.

### shared-daemon

- Telos: one shared app server owns all conversations, and installing a release never interrupts work.
- Holds: the TUI refuses an embedded app server unless `CODEX_ALLOW_EMBEDDED_SERVER` is set; SIGHUP, which `codex-app-server-refresh.service` sends when the installed release changes, defers replacement until running turns and admission permits are idle while resumes and new work are still admitted; explicit shutdown stays forceable; systemd kills surviving tool descendants once the daemon exits (`KillMode=mixed`).
- Owners: `codex-rs/tui/src/lib.rs`, `codex-rs/app-server/src/shutdown.rs` (`ShutdownState`), `codex-rs/app-server/src/turn_admission.rs`, `scripts/systemd/codex-app-server.service`, `scripts/systemd/codex-app-server-refresh.path`, `scripts/systemd/codex-app-server-refresh.service`.
- Tripwire: `shutdown_waits_for_admitted_requests_but_force_remains_available` and `pending_upgrade_keeps_admission_open_until_requests_and_turns_are_idle` (`codex-rs/app-server/src/shutdown_tests.rs`), `websocket_transport_pending_upgrade_allows_resume_and_new_turns_until_idle` (`codex-rs/app-server/tests/suite/v2/connection_handling_websocket_unix.rs`). The embedded-server refusal (compiled out of tests) and the unit files: none.
- Pressure: 0.162 #50803 (managed daemon for remote-control launches) and #50913 edit `tui/src/lib.rs`, which takes 9 upstream commits; #51470 edits `app-server/src/lib.rs`.
- Evidence: 2df4543ce0, 5aa88dcd33, 43a0125c85.

### reload-host

- Telos: the TUI process can be replaced without losing input or its server connection.
- Holds: `/reload` and idle restart hand `RetainedInput` to the new UI generation without detaching the server connection; a candidate is admitted only after its probe reports a checkpoint reader that reads the current checkpoint; handoff and rollback share one deadline; a rejected or stalled candidate never consumes launch input.
- Owners: `codex-rs/tui/src/reload/mod.rs`, `codex-rs/tui/src/reload/host.rs`, `codex-rs/tui/src/reload/retained.rs` (`RetainedInput`), `codex-rs/tui/src/reload/admission.rs`, `codex-rs/tui/src/reload/deadline.rs`, `codex-rs/tui/src/app/ui_reload.rs`, `codex-rs/tui/src/app/idle_restart.rs`, `codex-rs/cli/src/reload_probe.rs`.
- Tripwire: `live_reload_retains_steering_and_times_out_without_replaying_input` (`codex-rs/tui/tests/suite/ui_reload.rs`), `stalled_and_rejected_workers_share_the_original_deadline` (`codex-rs/tui/src/reload/deadline_tests.rs`), `incompatible_or_stalled_probe_never_runs_launch_input` (`codex-rs/tui/src/reload/worker_process_tests.rs`), the unit tests in `codex-rs/tui/src/reload/control.rs`.
- Pressure: owners are fork-owned; the seams in `cli/src/main.rs` (#50803, #50437, #49939) and `tui/src/lib.rs` change in 0.162.
- Evidence: 183bbf0d06, fac940a210, ab654c8333.

### server-hosted-agents

- Telos: agent coordination lives in the app server, so it survives frontends and restarts.
- Holds: task tools run in the app server, not the TUI; deliveries to a busy thread queue durably with attribution and never become user input; subagent families survive restarts, and only an idle subtree can be retired; farm requests persist; an active root thread's canonical name is unique; each thread's comms gate and context role are server state. A farm reservation is keyed by request identity before path allocation; only proven rejection releases the reserved path. Allocated or uncertain outcomes retain custody and are not silently retried. Cross-provider automatic farming defaults to Fresh independently of per-provider executor configuration.
- Owners: `codex-rs/app-server/src/thread_tools/mod.rs`, `codex-rs/app-server-protocol/src/delegation.rs`, `codex-rs/app-server/src/request_processors/thread_queue_processor.rs`, `codex-rs/ext/queue/src/service.rs`, `codex-rs/core/src/agent/control.rs`, `codex-rs/core/src/agent/control/retirement.rs`, `codex-rs/core/src/tools/handlers/multi_agents_v2/retire_agent.rs`, `codex-rs/state/src/runtime/farm_requests.rs`, `codex-rs/state/src/runtime/thread_names.rs`, `codex-rs/app-server/src/request_processors/comms_gate.rs`, `codex-rs/protocol/src/comms_gate.rs`, `codex-rs/app-server/src/request_processors/context_role.rs`, `codex-rs/protocol/src/context_role.rs`. Reservation authority: `codex-rs/core/src/agent/control/farm_reservation.rs`.
- Tripwire: `codex-rs/app-server/src/thread_coordination_lifecycle_tests.rs`, `codex-rs/app-server/tests/suite/v2/server_coordination_*tests.rs`, `queued_tool_output_waits_for_completion_without_becoming_user_input` (`codex-rs/app-server/tests/suite/v2/thread_queue.rs`), `v2_family_survives_unload_until_idle_subtree_retirement` (`codex-rs/core/src/agent/control_tests.rs`), `farm_spawns_and_replays_without_steering_parent` (`codex-rs/app-server/tests/suite/v2/thread_farm.rs`), `root_name_claim_race_and_archive_lifecycle` (`codex-rs/state/src/runtime/thread_names_tests.rs`), `closed_idle_destination_bounces_messages_until_reopened` and `context_role_is_discoverable_until_cleared`.
- Pressure: 0.162 edits `core/src/agent/control.rs` with #51415 (turn lineage), #51329 (no partial-history subagent forks), #50087 (queued agent mail across eviction), #49814 and #51515 (coordinated agent-tree shutdown); #51415 also edits `thread_queue_processor.rs`. Path allocation and replay must not mistake an uncertain accepted request for a fresh reservation.
- Evidence: fa1cf4ce29, 8e8bb48075, c8dec1e3eb, 217bcce455, db62274156, eb8132ec04, f4588c72c6, e373b1520b, e331fec3c1, 2d9abac7cf.

### authoritative-queue-integrity

- Telos: corruption cannot silently erase accepted mail or reopen its authoritative queue as usable.
- Holds: a corrupt queue database rejects initialization and remains untouched, without backup-and-rebuild. Repeated and concurrent initializers sharing a configuration observe the same integrity verdict for that file identity; a cached attempt alone never authorizes acceptance. Other runtime databases retain their own recovery policies.
- Owners: `codex-rs/state/src/sqlite.rs` (`QUEUE_DB`, `SqliteConfig::open_read_write_pool_with_spec`), `codex-rs/state/src/sqlite/queue_integrity.rs` (`QueueIntegrity`, `SqliteConfig::check_runtime_integrity`).
- Tripwire: `runtime_opens_recover_or_report_corruption_by_database_policy` (`codex-rs/state/src/sqlite/validation_tests.rs`) checks repeated rejection, unchanged corrupt bytes and no recovery backup. Concurrent initialization is serialized by the shared result cell; no separate concurrency witness.
- Pressure: upstream's `SqliteQuickCheckManager::quick_check_once` caches attempts, including corruption, and later returns `Skipped`; restoring that call admits a queue already rejected by the fork. Upstream's queue recovery policy rebuilds the database, which has no canonical source for accepted mail.
- Evidence: 387ab6b20b.

### idle-mcp-human-consent

- Telos: direct client MCP calls retain human consent while their thread is idle, without inventing automated review authority.
- Holds: an elicitation with no invocation identity and no active turn routes to the attached human, never Guardian or automatic acceptance. Current MCP runtime policy may reject it, and normal frontend form eligibility still applies; saved future-turn settings supply no review authority. An empty subscription snapshot declines immediately; loss of the last subscriber declines a pending idle request and releases its permission guard. Invocation-backed and active-turn requests retain their issuing authority and cancellation checks.
- Owners: `codex-rs/core/src/session/mcp_elicitation_authority.rs`, `codex-rs/codex-mcp/src/elicitation_authority.rs`, `codex-rs/codex-mcp/src/elicitation_policy.rs`, `codex-rs/app-server/src/idle_mcp_elicitation.rs`; seams in `codex-rs/codex-mcp/src/elicitation.rs`, `codex-rs/app-server/src/outgoing_message.rs` and `codex-rs/app-server/src/request_processors/thread_lifecycle.rs`.
- Tripwire: the ten idle elicitation cases in `codex-rs/app-server/tests/suite/v2/mcp_tool.rs`, `mcp_elicitation_reviewer_keeps_issuing_step_authority` (`codex-rs/core/src/session/tests.rs`), and `cancel_requests_for_thread_cancels_all_thread_requests` (`codex-rs/app-server/src/outgoing_message.rs`).
- Pressure: MCP review-policy changes must distinguish missing turn authority from refusal of human consent; frontend loss must never imply approval or an unanswerable wait.
- Evidence: 55bfd34783; owner decision during the 2026-10-29 test-suite repair campaign.

### durable-approvals-and-queues

- Telos: a reconnect loses no pending decision or queued edit.
- Holds: an unresolved approval request survives transcript eviction and replays after buffered events; reconnect restores pending edits as drafts without submitting them. The UI's operation ledger owns dispatch, thread-or-connection origin, captured store/epoch, ordered configuration barriers and completion. Unsent or definitively rejected drafts are recoverable; unknown effects are never replayed or reported as success. Urgent interrupts, response delivery and exit control bypass ordinary submissions. Navigation retains resource-producing requests after cancellation and cleans known uninstalled subscriptions through the original connection. Only confirmed detachment changes attachment state and recovers unsent work. Accepted modal consent retains its exact write and continuation behind the configuration barrier; supersession cannot cancel the write. Directory replacement revalidates the source, input and effective permissions, then detaches the captured source before its remaining graph. Permission confirmation does not revoke unchanged authority. Ordinary forks retain source-family input and approvals; safety retries require explicit authorization, fork before the interrupted turn and preserve its input and permissions.
- Owners: `codex-rs/tui/src/app/pending_interactive_replay.rs`, `codex-rs/tui/src/app/thread_event_buffer.rs`, `codex-rs/tui/src/app/queued_messages.rs` (`QueuedMessages`), `codex-rs/tui/src/app/reconnect.rs`. `codex-rs/tui/src/app/turn_submissions.rs` (`ServerOperations`), `codex-rs/tui/src/app/turn_submission_controller.rs`, `codex-rs/tui/src/app/thread_routing.rs`, `codex-rs/tui/src/app/server_operation_requests.rs`, `codex-rs/tui/src/app/server_realtime_operations.rs`, `codex-rs/tui/src/app/server_control_operations.rs`, `codex-rs/tui/src/app/server_navigation_operations.rs`, `codex-rs/tui/src/app/server_navigation_cleanup.rs`, `codex-rs/tui/src/app/server_detach_operations.rs`, `codex-rs/tui/src/app/server_directory_navigation.rs`, `codex-rs/tui/src/app/server_fork_navigation.rs`, `codex-rs/tui/src/app/server_navigation_permissions.rs`.
- Tripwire: `pending_approval_survives_transcript_eviction` (`codex-rs/tui/src/app/tests/thread_delivery_tests.rs`); `stalled_submission_does_not_block_queued_edit_or_typing` (`codex-rs/tui/src/app/tests/queued_message_tests.rs`) composes response, control, draft, consent, configuration and canceled-acquisition custody; `changing_directory_preserves_project_trust_permissions_history_and_hooks` (`codex-rs/tui/src/app/tests/session_lifecycle_requests.rs`); `safety_retry_preserves_unconfirmed_permissions_on_its_first_turn` (`codex-rs/tui/src/app/tests/safety_buffering.rs`). Queued edits across a reconnect: none.
- Pressure: 0.162 #51415 edits `pending_interactive_replay.rs`; `reconnect.rs` takes 7 upstream commits (#49472, #49715, #49809, #49857, #50148, #50199, #51500).
- Evidence: b56191783c, 2926fc7aa0, 0aaf58f10c, c201932da2, 9a8bc53195, 8773e03b0d, 13c6bad38d, 6bc6d40a59, 64a630c9f0, cbed4dd438, 186954aa13.

### goal-boxes

- Telos: a user can bound how long, or how far into its provider quota, a root agent pursues a goal, and the bound holds without a frontend.
- Holds: only a root agent's unfinished goal holds boxes. When its timebox ends or its quota falls to its quotabox, the server pauses the active goal, injects the halt prompt into a running turn and warns the thread's clients; idle continuation never starts past a reached box, and no activation succeeds while one stays reached. Rewording keeps boxes and usage; completion, through a database trigger, and clearing end them. An unreadable quota fails open with one warning per lapse.
- Owners: `codex-rs/ext/goal/src/boxes.rs`, `codex-rs/ext/goal/src/runtime/boxes.rs`, `codex-rs/ext/goal/src/api/boxes.rs`, `codex-rs/state/src/runtime/goals/boxes.rs`, `codex-rs/state/goals_migrations/0003_thread_goal_boxes.sql`, `codex-rs/protocol/src/goal_boxes.rs`, `codex-rs/app-server-protocol/src/protocol/v2/goal_boxes.rs`, `codex-rs/app-server/src/request_processors/goal_box_host.rs`, `codex-rs/app-server/src/request_processors/thread_goal_boxes.rs`, `codex-rs/tui/src/goal_boxes.rs`, `codex-rs/tui/src/app/thread_goal_actions/boxes.rs`; prompt text remains in `codex-rs/ext/goal/templates/goals/`.
- Tripwire: `boxes_outlive_rewording_and_end_with_completion` (`codex-rs/state/src/runtime/goals/boxes.rs`), `timebox_reminders_halve_the_time_left_until_ten_minutes_remain` and `embedded_box_prompts_render` (`codex-rs/ext/goal/src/boxes.rs`), `goal_box_grammar` (`codex-rs/tui/src/goal_boxes.rs`); no test drives a halt end to end.
- Pressure: upstream changes to idle continuation and activation in `codex-rs/ext/goal/src/runtime.rs` and `codex-rs/ext/goal/src/api.rs` must keep the halt checks; `thread_goals` writes must update rows in place, never replace them, or boxes vanish; goal migrations share upstream's version sequence, so a merge may renumber `0003_thread_goal_boxes.sql`.
- Evidence: 788025de66.

## Resilience policy

### compaction-is-not-input

- Telos: compaction preserves enough handling evidence to continue without repeating already-handled or superseded work.
- Holds: a bounded factual record comes from the frozen compaction input, retaining recent requests, explicit goal changes, assistant reports and paired tool receipts. Acknowledgment is not completion and missing evidence proves nothing. Only host annotations authenticate a retained record; quoted user/tool wrappers do not. Replacement retains concurrent history, and malformed authenticated evidence returns an error without replacing history. Persisted record framing does not depend on reminder wording. Refreshed standing instructions remain separate from this evidence.
- Owners: `codex-rs/core/src/context/compaction_continuation.rs`, `codex-rs/core/src/compact.rs`, `codex-rs/core/src/compact_remote_v2.rs`, `codex-rs/core/src/compact_native.rs`.
- Tripwire: `native_checkpoint_swap_retains_concurrent_tail_and_rejects_stale_prefix_atomically` (`codex-rs/core/src/session/native_compaction_tests.rs`), `remote_compact_v2_retains_metadata_from_resumed_history` and the existing goal/guardian compaction interactions.
- Pressure: upstream summary-only replacement, retained-history filtering or provider-specific checkpoint installation must not turn compaction into new user intent or discard the host-authenticated continuation.
- Evidence: 2387ddf521, fabf777aac.

### backpressure-waits

- Telos: provider backpressure delays interactive work instead of failing it.
- Holds: `ServerOverloaded`, `RateLimitExceeded` and a 429 that is neither a usage limit nor exhausted quota make an interactive session wait as long as the user allows, without spending the retry budget; server advice sets the delay, otherwise it doubles from 5 s to 60 s; usage limits and exhausted quota end the turn; internal and Guardian sessions keep their failure path; Anthropic overload, rate-limit and API errors stay retryable wherever they arrive in a stream.
- Owners: `codex-rs/core/src/backpressure.rs`, `codex-rs/core/src/responses_retry.rs`, `codex-rs/codex-api/src/anthropic/decoder.rs`, `codex-rs/codex-api/src/anthropic/client.rs`.
- Tripwire: `responses_http_capacity_recovers_and_remains_interruptible`, `sse_rate_limit_without_retry_after_outlasts_stream_retries`, `sse_capacity_recovers` and `websocket_backpressure_recovers` (`codex-rs/core/tests/suite/retry_after.rs`), `stream_failures_stay_typed_for_retry` (`codex-rs/codex-api/src/anthropic/stream_tests.rs`).
- Pressure: 0.162 #49441 retries `ServerOverloaded` with server advice "within the configured retry budgets" in `responses_retry.rs`.
- Evidence: f36483414d, 076db2fb39.

### compaction-stops-on-uncertainty

- Telos: a compaction that may have succeeded, or failed for good, is not repeated on another model.
- Holds: `should_retry_with_current_model` is false for `Fatal`, `TurnAborted`, `Interrupted` and `SessionBudgetExceeded`; `compact_remote_v2` returns such an error, including one from the fallback attempt, instead of retrying with the current model.
- Owners: `codex-rs/core/src/compact_model_fallback.rs` (`should_retry_with_current_model`), `codex-rs/core/src/compact_remote_v2.rs`.
- Tripwire: none; its test left with `compact_remote_v2_native_tests.rs` in d68f5c1e02.
- Pressure: 0.162 #51117 edits `compact_model_fallback.rs` and `compact_remote_v2.rs`.
- Evidence: a8b367fbfe.

### alarm-watched-sessions

- Telos: a background session that an alarm watches lives until the alarm resolves; idle pruning never decides the outcome an alarm is waiting for.
- Holds: at the unified-exec process cap, a process with an armed terminal watch is never pruned, however long it has been idle; disarming, a match or the process's exit releases the exemption, and watched sessions may push the store past its soft cap. Pruning a live process logs its id, idle time and command.
- Owners: `codex-rs/core/src/unified_exec/terminal_watch.rs` (`TerminalWatches::armed`, `TerminalWatch::arm`), `codex-rs/core/src/unified_exec/process_manager.rs` (filter seam in `prune_processes_if_needed`, prune log in `store_process`).
- Tripwire: `armed_terminal_watch_exempts_its_session_from_pruning` (`codex-rs/core/src/unified_exec/process_manager_tests.rs`).
- Pressure: upstream's LRU pruning (`prune_processes_if_needed`, `process_id_to_prune_from_meta`) ranks candidates by `last_used`; a silent waiter is never touched, so restoring upstream's candidate list makes it the first live process terminated, as `alarm.set` recommends exactly that pattern for long waits.
- Evidence: d160c7469e; libgrid sessions 25101 and 27493 terminated with exit 143, 2026-10-29.

## Accounts

### interleaved-turn-accounting

- Telos: every completed inference is charged without moving its tokens into another turn.
- Holds: completed speculative compaction is charged once even when its checkpoint is discarded. A persisted turn subtotal accumulates only that turn's responses; a late background response from an older turn cannot reset the foreground subtotal. The thread total remains cumulative. Foreground and background contexts share a turn-owned subtotal cell, released with the last context; cold replay retains at most one seed for the last started turn and never searches past a compaction checkpoint.
- Owners: `codex-rs/core/src/state/token_accounting.rs`; seams in `state/session.rs`, `state/mod.rs`, `session/turn_context.rs` and `session/mod.rs` under `codex-rs/core/src/`.
- Tripwire: `async_compaction_reuses_pending_work` (`codex-rs/core/tests/suite/scenarios.rs`); `record_token_usage_continues_restored_totals` (`codex-rs/core/src/state/session_tests.rs`).
- Pressure: speculative requests can complete after a newer turn reports usage. Deriving the turn subtotal from the latest thread record loses the newer turn's earlier responses; an unbounded turn-indexed map instead leaks closed turns.
- Evidence: 21441bcbc0; 40de654f41 charges completed background responses; test-suite repair, 2026-10-29, exposed the interleaving defect.

### pinned-accounts

- Telos: each conversation bills the account the human chose, and switching never breaks work in progress.
- Holds: every conversation and family is pinned to an account; a switch applies per session at the next safe request boundary without interrupting tools or inference; a switch moves only the selected route and never revokes: admitted work keeps its account until logout, principal replacement or permanent refresh failure revokes that account, and a captured source refreshes only for its own principal; quota snapshots are shared through the server, coalesced, and honor per-account cooldowns; a fresh draft reads quota for the account it displays. Agent registration compare/exchanges metadata into the captured account and publishes only identity metadata to its cached auth, never changed stored credentials. Loaded managed-workspace auth is excluded from personal-account selection before consulting secure storage; personal-account defaults retain persisted-login freshness.
- Owners: `codex-rs/core/src/session/account.rs`, `codex-rs/app-server/src/request_processors/thread_accounts.rs`, `codex-rs/app-server/src/request_processors/account_processor/quota.rs`, `codex-rs/app-server/src/request_processors/account_processor/quota_cache.rs`, `codex-rs/app-server/src/request_processors/account_processor/account_scope.rs`, `codex-rs/tui/src/app_server_session/account.rs`, `codex-rs/login/src/auth/manager.rs` (`persist_agent_identity_record`), `codex-rs/login/src/auth/manager/identity_persistence.rs`, `codex-rs/login/src/auth/manager/account_scope.rs` (`AuthManager::active_account_id`).
- Tripwire: `conversation_accounts_survive_selection_login_and_fork` (`codex-rs/app-server/tests/suite/v2/thread_accounts.rs`), `cancelled_clients_cannot_erase_refreshes_or_failure_cooldowns` (`codex-rs/app-server/src/request_processors/account_processor/quota_cache_tests.rs`). `chatgpt_auth_registers_agent_identity_when_enabled` (`codex-rs/login/src/auth/auth_tests.rs`) checks registration when a legacy client omits login metadata; changed-grant registration has no separate witness. `plan_type_maps_enterprise_cbp_usage_based_plan` (`codex-rs/login/src/auth/auth_tests.rs`) checks managed-account selection with unavailable storage; `enterprise_login_attempt_lifecycle` (`codex-rs/app-server/tests/suite/v2/account_enterprise_tests.rs`) gates primary keyring access while starting a thread. Draft quota scope: none. `account_grant_tracks_refresh_replacement_and_logout` (`codex-rs/login/tests/suite/auth_refresh.rs`) checks that admitted work keeps its account across a switch and loses it at that account's logout.
- Pressure: 0.162 #50199 (account email in `/status`) and #49715 (account security reminders) edit the TUI account paths in `app.rs`, `reconnect.rs` and `lib.rs`. Registration persistence must retain account-scoped compare/exchange and identity-only cache publication instead of restoring a store save/reload. Preserve the managed-workspace exclusion before storage lookup. Personal-account lookup still reads secure storage synchronously to observe out-of-process login changes; removing that read requires a replacement freshness owner. 0.169 transports check scope generations, which exposed HEAD invalidating admitted work on a selector switch; selection publication stays separate from revocation.
- Evidence: 8bc33e5acd, 0ec65fe8c4, df24f7a816, 9733575fc3, 901f783aff, ade2c0c496; 16e9d63de2 separates selection from revocation and binds Guardian review catalogs to the admitted account.

### recoverable-sign-in

- Telos: expired provider sign-ins name the account and reconnect retained conversations after sign-in.
- Holds: dead subscription grants remain typed through catalog discovery, session start/resume, turn events and JSON-RPC error data; only sign-in-required errors bypass best-effort catalog fallback. The TUI preserves transcripts and drafts, offers `/account`, and resumes affected conversations after successful provider sign-in, while headless clients receive the provider-specific login command. ChatGPT refresh handling is unchanged.
- Owners: `codex-rs/protocol/src/sign_in_required.rs`, `codex-rs/login/src/anthropic/manager.rs`, `codex-rs/model-provider/src/anthropic_auth.rs` and `native_claude_catalog.rs`, `codex-rs/models-manager/src/authoritative_catalog.rs`, `codex-rs/core/src/session/model_catalog.rs`, `codex-rs/app-server/src/request_processors/sign_in_required.rs`, `codex-rs/tui/src/app/sign_in_required.rs` and `reconnect.rs`.
- Tripwire: `failed_refresh_is_account_local_and_native_login_supersedes_its_grant`, `authoritative_catalog_access_rejection_invalidates_cache_until_refresh_succeeds`, `claude_subscription_accounts_are_native_and_conversation_scoped`, the active/navigation reconnect tests and `live_app_server_failed_turn_does_not_duplicate_error_history`.
- Pressure: upstream best-effort catalogs and request-error formatting discard structured causes; reconnect fallback treats all unavailable transcripts alike.
- Evidence: 5109352165.

### provider-login

- Telos: signing in chooses the provider instead of silently selecting OpenAI.
- Holds: bare login offers ChatGPT and Claude subscription only when stdin and stdout are terminals; unattended login requires an explicit provider or an OpenAI credential/device-auth option. Explicit provider selection skips the menu. Unqualified status reports OpenAI and saved Claude accounts, even when one provider has no login or its storage fails; explicit status retains its provider-specific behavior. OpenRouter remains a key-file account choice, not a sign-in flow.
- Owners: `codex-rs/cli/src/login_command.rs`, `codex-rs/cli/src/login_status.rs`, `codex-rs/cli/src/claude_login.rs`, with dispatch and status seams in `main.rs` and `login.rs`.
- Tripwire: `login_with_api_key_reads_stdin_and_writes_auth_json`, `login_status_reports_auth_storage_errors`, and `device_login_adds_an_account_without_revoking_existing_credentials` in `codex-rs/cli/tests/login.rs`.
- Pressure: upstream's bare login and single-provider status assume OpenAI; prompt logic placed in the command tree smears the fork across upstream dispatch.
- Evidence: 742a56de48.

### credential-backend-authority

- Telos: credential instrumentation cannot replace a backend's storage and transaction authority.
- Holds: `ObservedStorage` delegates `save` and `compare_exchange` to the inner backend while recording logical telemetry. Ephemeral transactions remain atomic and entirely in memory: they create neither a Codex home nor lock files. An inherited trait default cannot substitute filesystem locking or a split read/write for a backend override.
- Owners: `codex-rs/login/src/auth/storage_telemetry.rs` (`ObservedStorage`), `codex-rs/login/src/auth/storage.rs` (`AuthStorageBackend`, `EphemeralAuthStorage`).
- Tripwire: `ephemeral_storage_save_load_delete_is_in_memory_only` (`codex-rs/login/src/auth/storage_tests.rs`) checks successful and stale compare/exchange through the observed backend and that an absent home remains absent.
- Pressure: the fork's transactional storage methods have defaults; adding or restoring a trait method without forwarding its backend override can silently reintroduce disk effects or bypass atomicity. Review the decorator whenever `AuthStorageBackend` gains an operation, not only when telemetry changes.
- Evidence: 8b8b30b3c4.

## Repository policy

### policy-owned-worktrees

- Telos: checkout policy belongs to each repository, not to a competing Git worktree manager inside Codex. Agents use ordinary Git commands under the user's and repository's instructions.
- Holds: Codex neither allocates nor deletes managed Git worktrees, offers worktree commands or a browser, nor couples conversation creation or `/fork` to checkout creation. Conversation history forks, ordinary working-directory selection and execution of Git commands remain supported. Branch choice, detached state, checkout placement and retention are repository policy.
- Owners: `Feature::Worktrees` is a removed capability. No dedicated worktree crate, TUI lifecycle service, browser, checkout picker, CLI option or model tool remains. Unrelated Git inspection and generic attachment support remain.
- Tripwire: worktree commands and dependencies must not return. Conversation forks and ordinary cwd selection retain history, trust, permissions and background-terminal safeguards; thread pickers match the chosen cwd rather than expanding it to every linked checkout.
- Pressure: 0.162 #50148 adds model-facing managed-worktree tools hosted by the TUI; #50416 expands checkout choices for new and forked conversations. Neither should restore a product-owned worktree lifecycle.
- Evidence: maintainer decision on 2026-10-11 during 0.162.1 merge triage; CLI-wide locked checks and focused exact-cwd picker checks after removal.

### no-terminal-pets

- Telos: terminal companions cost neither rendering complexity nor recurring merge work.
- Holds: no pet picker, assets, animation, terminal-image rendering, settings or slash command remains. Old `[tui]` pet keys are ignored by serde, like other unknown preferences; the schema no longer advertises them.
- Owners: none. Deleted prefixes: `codex-rs/tui/src/pets/`, `codex-rs/tui/src/app/pets.rs`, `codex-rs/tui/src/chatwidget/pets`; pet-only snapshots are deleted too. Resolve modify/delete conflicts with `git rm`.
- Tripwire: `python scripts/check_removed_capabilities.py --check`; omit `--check` after a merge to re-delete restored capability files and report wiring that needs removal.
- Pressure: nine release merges from `rust-v0.153.4` to `rust-v0.162.1`: estimated 0.6 modify/delete conflicts, 0.1 orphan files and 1.2 seam files per merge after this cut.
- Evidence: maintainer capability-cut decision; existing TUI configuration-default case also exercises ignored retired keys.

### no-ide-context-capture

- Telos: the terminal does not control or query an IDE.
- Holds: `/ide`, IDE IPC/context collection and its status indicator are absent. Rendering and title generation still recognize context-prefixed history supplied by other clients; `codex-rs/tui/src/prompt_request.rs` owns that shared replay delimiter.
- Owners: none for capture. Deleted prefixes: `codex-rs/tui/src/ide_context`, `codex-rs/tui/src/chatwidget/ide_context.rs`; its footer snapshot is deleted too. Resolve modify/delete conflicts with `git rm`.
- Tripwire: `python scripts/check_removed_capabilities.py --check` (or omit `--check` to reapply file deletions).
- Pressure: nine release merges from `rust-v0.153.4` to `rust-v0.162.1`: estimated 0.4 modify/delete conflicts per merge, without recurring seams.
- Evidence: maintainer capability-cut decision; existing replay/display tests retain cross-client history behavior.

### no-external-agent-import

- Telos: importing another agent's settings and sessions costs no runtime or build graph.
- Holds: `/import`, migration screens, app-server handlers and state import-record APIs are absent. The four existing external-agent RPC methods answer an unsupported error; app-server-protocol and shared `codex-analytics` emitter/event/fact/reducer code stay unchanged to avoid recurring upstream seams; no built import path calls those emitters. Both applied import SQL migrations remain because SQLx validates migration history. `Feature::ExternalAgentMemoryImport` and `Feature::ExternalMigration` are removed.
- Owners: none. `codex-rs/external-agent-migration/` stays whole and unbuilt, explicitly excluded from the workspace and absent from dependencies and Cargo.lock; upstream edits there merge normally. Deleted prefixes: `codex-rs/tui/src/external_agent_config_migration/`, `codex-rs/tui/src/app_server_session/external_agent_config`, `codex-rs/app-server/src/external_agent_migration/`, `codex-rs/app-server/tests/suite/v2/external_agent`, `codex-rs/state/src/runtime/external_agent_config_imports`. Resolve modify/delete conflicts with `git rm`, not partial orphan modules.
- Tripwire: `python scripts/check_removed_capabilities.py --check` (or omit `--check` to reapply file deletions); the existing `legacy_permission_profile_requests_fail_closed` integration case also checks all four unsupported methods.
- Pressure: deleting the migration crate would add about 2.3 modify/delete conflicts per merge across the nine releases from `rust-v0.153.4` to `rust-v0.162.1`; retaining it unbuilt avoids those conflicts.
- Evidence: maintainer capability-cut decision; protocol schemas and state migration history are retained verbatim.

### no-feedback-upload

- Telos: routine diagnostics never reach disk or a vendor; they stay in a bounded in-memory flight recorder that still answers while a thread is wedged.
- Holds: `/feedback`, its views, uploads, saves, archives, feedback tags, guardian failed-review capture and the SQLite log sink are absent. `feedback/upload` and `feedback/save` answer an unsupported error; their protocol and schema stay unchanged. `codex-feedback` is an inert stub: `CodexFeedback` carries nothing and its tag emitters discard. `codex_state::log_db::LogDbLayer` is uninhabited, so every `Option<LogDbLayer>` is `None`, and `feedback_tags!` borrows and discards. No runtime opens `logs_2.sqlite`: its spec, migrations, `log_dir` placement, directory lock and the background page reclamation only it enabled are deleted, and an existing file is left for its owner. The guardian review table stays in the state migrations because SQLx validates migration history. `codex-flight-recorder` owns one ring per process with separate byte budgets for WARN+, INFO and DEBUG+TRACE (server 1/5/10 MiB, clients 0.5/1.5/2 MiB). It is read through the unserialized `debug/flightRecorder/read` RPC or `codex logs` (`--follow` polls by sequence number), and written to `$CODEX_HOME/log/flight-recorder/` only on SIGUSR1, a panic, or as a forced app-server shutdown begins, keeping the newest four dumps per cause. The ring lock never spans tracing, I/O or awaits.
- Owners: `codex-rs/flight-recorder/`, `codex-rs/app-server/src/request_processors/flight_recorder.rs`, `codex-rs/app-server-protocol/src/protocol/v2/flight_recorder.rs`, `codex-rs/cli/src/logs_cmd.rs`. Deleted prefixes live in `scripts/check_removed_capabilities.py`; resolve modify/delete conflicts with `git rm` and keep the stubs on modify/modify.
- Tripwire: `python scripts/check_removed_capabilities.py --check`; the guardian v2 user-input cases check thread attribution through a recorder query, and `suite::logging` checks submission metadata and credential redaction through it.
- Pressure: seven release merges from `rust-v0.159.1` to `rust-v0.166.0` touched 14 `feedback/` files, 5 app-server feedback files, 3 log-database files and 1 TUI feedback view: an estimated 3–4 modify/delete conflicts per merge. The stubs keep about 40 plumbing files merging without seams.
- Evidence: maintainer decision 2026-11-06. Before the cut the log database was the server's largest write stream (3.2 MB/min of WAL against 0.24 MB/min of rollout appends).

## Disk economy

### no-cloud-connection

- Telos: every conversation runs in the shared server under server-owned coordination; a second, cloud-hosted thread backend would bypass it and costs merge surface.
- Holds: no Codex Cloud stack remains: no `codex-cloud-client` crate, no Cloud backend in `app-server-client`, no TUI `Cloud` thread target, Cloud project selection or switch wait; no `codex cloud` command, cloud-tasks crates or the TUI public widgets that only they consumed. `cloud-config` remains: the app server, CLI, `codex exec` and the TUI read it.
- Owners: none. Deleted prefixes: `codex-rs/cloud-client/`, `codex-rs/app-server-client/src/cloud.rs`, `codex-rs/app-server-client/src/cloud_backend/`, `codex-rs/app-server-client/src/cloud_tests.rs`, `codex-rs/tui/src/app/cloud_switch_wait.rs`, `codex-rs/tui/src/app_server_session/cloud_project`, `codex-rs/cloud-tasks/`, `codex-rs/cloud-tasks-client/`, `codex-rs/cloud-tasks-mock-client/`, `codex-rs/cli/tests/cloud_auth.rs`, `codex-rs/tui/src/public_widgets/`. Resolve modify/delete conflicts with `git rm`.
- Tripwire: `python scripts/check_removed_capabilities.py --check` covers the deleted prefixes, `codex_cloud_client`, `codex_cloud_tasks`, `public_widgets` and Cloud-target references, workspace membership, manifests and `Cargo.lock`.
- Pressure: `rust-v0.169.0` brought 24 PRs for the opt-in backend (TUI target, model and effort selection, archive/restore, runtime auth); later releases will extend it.
- Evidence: 16e9d63de2 removes the stack; owner decisions D5, 2026-11-14 (Cloud connection) and 2026-11-15 (`codex cloud`).

### framed-rollouts

- Telos: a rollout costs its compressed size on disk from its first line, with no cold-file pass and no plain copy for readers.
- Holds: new rollouts are written only as `.jsonl.zst` in the framed encoding. The file is a sequence of standard zstd frames (level 3, window log 23, XXH64 checksum), each holding at most 16 MiB of logical bytes. Each frame is followed by a skippable trailer that records the frame start, logical start, logical length and the previous trailer. A 24-byte skippable hint at offset 0 names the last trailer. The writer flushes each acknowledged line to a block boundary and closes a frame at 16 MiB, after 10 idle minutes and on shutdown. When it reopens a file, it seals a torn frame at the last block that ends on a newline. Sealing never destroys bytes: whatever it cuts is first copied to a synced `<file>.torn-<unix ms>` sidecar. It logs at WARN for a torn tail and at ERROR when the cut bytes hold a later frame or a complete trailer, which is corruption. A writer exists only under its thread's cross-process writer lock, and it refuses to write, logging at ERROR, if the file's length changed under it. Framed appends, frame closes and opens run on the blocking pool. Every re-encoding (conversion, migration publish) decodes its synced output and fails unless it reproduces the SHA-256 of the bytes fed to the encoder. Conversion fsyncs the directory after the rename and only then removes a plain source. Readers decode by logical offset and walk the trailer chain once. Stored paths (`threads.rollout_path`, the API `path`, `RolloutRecorder::rollout_path`) keep the logical `.jsonl` name. `existing_rollout_path` resolves the file on disk and prefers a plain file. Plain and upstream single-frame files stay readable. Appending to a plain file keeps it plain; appending to a single-frame file converts it first. `codex rollouts compress` and the experimental `rollout/compress` RPC convert the rest, each file under its thread's writer lock. The app server logs a startup census. Upstream's cold-compression worker and `Feature::LocalThreadStoreCompression` are removed. Hooks receive the file on disk as `transcript_path`; for a framed rollout that file is zstd, which `zstd -dc` reads.
- Owners: `codex-rs/rollout/src/framed.rs`, `codex-rs/rollout/src/framed/`, `codex-rs/rollout/src/rollout_sink.rs`, `codex-rs/rollout/src/conversion.rs`, `codex-rs/rollout/src/test_support.rs`, `codex-rs/cli/src/rollouts_cmd.rs`. Seams in upstream files:
  - `rollout/src/compression.rs`: `resolve_rollout_for_append`, `convert_rollout_blocking`, and the framed dispatch in `reader::open_once`;
  - `recorder.rs`: `RolloutSink` and the idle frame close;
  - `seekable_reader.rs`, `ordinal.rs`, and `lib.rs` (`materialize_rollout_for_reference`);
  - `thread-store/src/local/history_revision.rs`: the logical-length revision;
  - `rollout_lineage.rs`, `live_writer.rs` and `rollout_migration/publish.rs`;
  - `core/src/thread_manager.rs` and `core/src/session/mod.rs` (`hook_transcript_path`);
  - `app-server/src/lib.rs` (the census) and `app-server/src/request_processors/rollout.rs`;
  - `tui/src/resume_picker_transcript_preview.rs` and `features/src/lib.rs`.
- Tripwire: `codex-rs/rollout/src/framed_tests.rs` (a crash-truncation and trailer-corruption property that also checks the quarantine holds exactly the cut bytes, a seek across frames, a concurrent reader and writer); `rollout_compress_converts_a_plain_rollout` (app server); `framed_shared_fork_resume_preserves_checkpoint_and_frozen_history` (core); `python scripts/check_removed_capabilities.py --check` for the worker's deletion and the removed feature.
- Pressure: upstream's compression worker (`mod worker` and `mod metrics` in `compression.rs`) and its materialization paths. Any upstream reader or test that opens the logical `.jsonl` path directly fails on a framed rollout. Route it through `existing_rollout_path`, `open_rollout_seekable_reader` or `read_rollout_text`; tests use `codex_rollout::test_support`.
- Evidence: maintainer decision 2026-11-06. Production held 5,123 plain rollouts (38.4 GB) and 1,169 single-frame ones (6.0 GB). A deterministic sample of 102 plain rollouts (461 MiB) converts to 81 MiB (5.7×) in 1.2 s. Flushing after every line, the worst case for a live writer, gives 83 MiB (5.55×). `zstd -dc` reproduces the converted files byte for byte.

### history-pruning

- Telos: the SQLite history projection holds only what is in use, because it can always be rebuilt from rollouts.
- Holds:
  - Eviction: a projection unused for 14 days is evicted. Use is `accessed_at_ms`, which projected writes set and which reads and resumes advance at most hourly; rows from before the column fall back to `threads.updated_at`. Evicting deletes the projected rows and the projection state row in one transaction, and never touches a session file. It never runs while the thread is loaded in this process or holds its cross-process writer lock.
  - Reads: every paginated read first rebuilds an absent projection, or finishes one marked `rebuilding`, under the per-rollout lock that every catch-up pass takes. So a rebuild in progress is never read as empty or truncated.
  - Reclamation: evicted pages are reclaimed in bounded `incremental_vacuum` chunks. A chunk searches the freelist for every page it frees, so while freed pages are at least half the file and live data is at most 6 GiB, a full VACUUM repacks the database instead; it also converts one still in `auto_vacuum=NONE`. A repack runs only with no projection write for 5 minutes, re-checked just before it starts, and is attempted at most daily. No other database is ever vacuumed.
- Owners: `codex-rs/thread-store/src/local/history_pruning.rs` (`ProjectionWarden`, `ensure_projected`, `evict`, `reclaim`, `repack`), `codex-rs/state/thread_history_migrations/0009_thread_history_lifecycle.sql`. Seams in upstream files:
  - `thread-store/src/local/mod.rs`: the module and the `projection_warden` field;
  - `thread_history.rs`: the use stamp and the `rebuilding` mark in the upsert;
  - `thread_history_materialization.rs`: the per-rollout lock and `finish_rebuild`;
  - `rollout_lineage.rs`: `ensure_projected`;
  - `live_writer.rs`: `touch`;
  - `core/src/thread_manager.rs`: `spawn_history_pruning`.
- Tripwire: `paginated_live_append_materializes_turn_items_and_state` (`codex-rs/thread-store/src/local/thread_history_materialization_tests.rs`) evicts, then checks that two concurrent page reads equal the page from before eviction and that `rebuilding` clears.
- Pressure:
  - A new upstream reader that queries the projection tables without going through `resolve_rollout_lineage_for_history_page` would read an evicted projection as empty.
  - A new projected table that `evict` does not delete would leave orphans.
  - Upstream catch-up changes that write outside `materialize_to_sqlite` would bypass the per-rollout lock.
- Evidence: maintainer decision 2026-11-06.

## Build economy

### one-dependency-graph

- Telos: `cargo test` reuses the units `cargo build` compiled instead of compiling a second copy.
- Holds: no dev-dependency changes the features of a normal build unit; `codex-feature-floor` raises any feature a dev-dependency would otherwise add.
- Owners: `codex-rs/feature-floor/Cargo.toml`, `scripts/check_feature_parity.py`, `codex-rs/Cargo.toml`.
- Tripwire: `scripts/check_feature_parity.py`, run by hand; `just test` does not run it.
- Pressure: every upstream dependency change; 0.162 edits `codex-rs/Cargo.toml` 9 times, including #50113 (native gRPC client) and #50166 (`age` 0.12.1).
- Evidence: dec3c727fc.

### pipelined-crate-graph

- Telos: an edit recompiles as few crates as possible and they compile in parallel.
- Holds: `codex_cli` is a library and the `codex` binary a shim; the TUI and `codex-app-server-client` reach the app server only through `codex-app-server-embedding`, so no standalone `codex-tui` binary exists; core suites spawn `codex-exec`, not `codex`; the `local` profile builds workspace crates at O1 and dependencies at O2.
- Owners: `codex-rs/cli/src/main.rs`, `codex-rs/cli/src/bin/codex.rs`, `codex-rs/app-server-embedding/src/lib.rs`, `codex-rs/app-server/src/embedding.rs`, `codex-rs/app-server-client/Cargo.toml`, `scripts/run_tests.py`, `codex-rs/Cargo.toml` (`[profile.local]`).
- Tripwire: none.
- Pressure: 0.162 #50803, #50437 and #49939 edit `cli/src/main.rs`; any upstream dependency from the TUI or `codex-app-server-client` on `codex-app-server` restores the serial edge.
- Evidence: e092722ce7, 1064ccccc6, fad9f8bc86, 639630bf4e.

### removed-stacks

- Telos: capabilities this workstation never uses cost no compile time and no merge surface.
- Holds: the workspace and `codex-rs/Cargo.lock` contain none of: native Amazon Bedrock providers, credentials and onboarding; the OpenTelemetry SDK and exporters (`opentelemetry` remains with `trace` only); the managed network-proxy transport and Rama; AWS SigV4 and `codex-aws-auth`; the voice host and its WebRTC and ALSA graph; terminal pets and their image-rendering path; TUI IDE-context capture; external-agent config/session import (the migration crate remains in-tree but unbuilt); `tcp-tunnel`; `responses-api-proxy`; `app-server-test-client`; arboard's Wayland backend; `thread-manager-sample` and `core-api`; Windows sandbox dependencies outside `cfg(windows)` targets. `sdk/typescript`, its npm dependency graph, build/test integration and publication artifact are removed. Shared TypeScript protocol generators, the Python SDK, Rust APIs, headless execution and CLI packaging remain. Shared MCP conformance fixtures remain in Python CI; the SDK-hosted official and production reviewer CI gates are retired. Manual conformance requires an externally provisioned runner and never bootstraps registry dependencies.
- Owners: none; the deletions leave seams in `codex-rs/Cargo.toml`, `codex-rs/cli/Cargo.toml`, `codex-rs/cli/src/main.rs`, `codex-rs/windows-sandbox-rs/Cargo.toml` and `codex-rs/mxc-sandbox/Cargo.toml`.
- Tripwire: none; needs a denylist check of the deleted directories, of `Cargo.lock` packages (`opentelemetry_sdk`, `opentelemetry-otlp`, `rama*`, `aws-sigv4`, `webrtc*`, `alsa`, `wayland*`) and of arboard features.
- Pressure: 42 files deleted on main are modified upstream between `rust-v0.160.0` and `rust-v0.162.0`, chiefly under `model-provider/src/amazon_bedrock`, `voice-host`, `tcp-tunnel` and `otel/tests`; each is a modify/delete conflict whose resolution is `git rm`. 0.162 #49836 adds voice microphone selection to `core/src/config/mod.rs`.
- Evidence: 94d7235008, 3f4d858769, 5f45f5bbc5, 0700355c3e, 7f2715c164, 60f6ba0573, e176b09c07, 6d7c491d67, bbf73666b7, 177ca0e1e2, d206c23b79, 262a393d8a.

## TUI doctrine

### client-only-shell

- Telos: a deliberate local terminal command need not enter the agent's conversation or wait for its server.
- Holds: only visible composer `!!` intent admits host-local execution; hidden pasted prefixes and attachments cannot acquire it. One admission lease owns the command before the draft is cleared, then transfers to the process job or retained rejection. Host cwd, local process-group custody, bounded output and cancellation stay separate from agent shell policy and conversation cwd. Startup recovery never turns local intent into agent input; newer drafts are not overwritten by failure recovery.
- Owners: `codex-rs/tui/src/local_shell.rs`, `codex-rs/tui/src/app/local_shell.rs`, `codex-rs/tui/src/bottom_pane/chat_composer/local_shell.rs`, `codex-rs/tui/src/chatwidget/local_shell.rs`.
- Tripwire: `pasted_input_preserves_app_boundary_and_local_shell_authority` (`codex-rs/tui/src/app/tests.rs`).
- Pressure: composer paste handling, submission/reconnect recovery and shell routing must preserve the explicit host-local authority boundary and single draft lease.
- Evidence: 865ddc1d21, 1103c818cc, 4995ef537e.

### explicit-command-verbs

- Telos: a mistyped command never becomes an instruction to execute or a value to store.
- Holds: a command with no subcommands may accept bare prose; if subcommands exist, all are explicit. Enumerated verbs reject unknowns before executing, queuing work, or changing goal, conversation, or draft state. Prose at a verb-bearing boundary requires its explicit verb. Bare menu/status affordances remain valid. `/goal set <objective>` alone enters the objective path; `/goal` opens the existing menu, and replacing an unfinished goal still requires confirmation. Use the existing typed enum/parsing machinery, not a second generic command framework.
- Owners: `codex-rs/tui/src/chatwidget/goal_command.rs` (`GoalCommand`, `ChatWidget::dispatch_goal_command`), `codex-rs/tui/src/chatwidget/context_role.rs` (`ContextRoleCommand`), `codex-rs/tui/src/chatwidget/pets_command.rs` (`PetCommand`), `codex-rs/tui/src/chatwidget/slash_dispatch.rs`, `codex-rs/tui/src/goal_display.rs`, `codex-rs/tui/src/slash_command.rs`, `codex-rs/exec/src/command_grammar.rs` (`Command`), `codex-rs/cli/src/sandbox_command.rs` (`SandboxSubcommand`); top-level CLI prose is already explicit through `codex --prompt` in `codex-rs/tui/src/cli.rs`.
- Tripwire: `goal_control_slash_commands_emit_goal_events`, `goal_slash_command_emits_attached_images`, `queued_goal_slash_command_emits_set_goal_event_after_thread_starts`, `restored_queued_goal_slash_command_emits_set_goal_event`, `slash_pets_with_arg_selects_named_pet` (`codex-rs/tui/src/chatwidget/tests/slash_commands.rs`), `context_role_command_output` (`codex-rs/tui/src/chatwidget/tests/context_role_tests.rs`), `goal_slash_command_emits_only_inserted_paste_text_element` (`codex-rs/tui/src/chatwidget/tests/goal_validation.rs`), `set_thread_goal_draft_materializes_long_objective_and_confirms_before_paste` (`codex-rs/tui/src/app/tests.rs`), `parses_config_isolation_flags` (`codex-rs/exec/src/cli_tests.rs`), stdin interactions (`codex-rs/exec/tests/suite/prompt_stdin.rs`), `unrecognized_words_are_rejected_rather_than_seeded_as_prompts` and `sandbox_uninstall_respects_command_separator` (`codex-rs/cli/src/main.rs`).
- Pressure: upstream `/goal` treats every unrecognized control word as a new objective; upstream's CLI also allows bare positional prompts. Preserve explicit verbs and the confirmation path when merging dispatch, help, or completion changes.
- Evidence: command-grammar owner decision, 2026-10-12.

### scoped-config-overrides

- Telos: splitting configuration flags across command verbs never drops earlier assignments or redirects a request to another provider endpoint.
- Holds: the multitool and standalone exec collect `-c` assignments from root to selected leaf; deeper assignments win. Clap owns argument and `--` boundaries. Dispatch retains child copies where they must override generated feature settings; replaying the same assignment is idempotent, not a second effect. The shared traversal covers app-server, MCP, sandbox and debug subtrees without separate command grammars.
- Owners: `codex-rs/utils/cli/src/scoped_config.rs`, `codex-rs/cli/src/config_override_args.rs`, `codex-rs/exec/src/command_args.rs`; entry-point seams in `codex-rs/cli/src/main.rs` and `codex-rs/exec/src/main.rs`.
- Tripwire: `preserves_scoped_overrides_and_forwarded_arguments` (`codex-rs/cli/src/config_override_args_tests.rs`), `top_cli_parses_resume_prompt_after_config_flag` (`codex-rs/exec/src/main_tests.rs`).
- Pressure: Clap's global Append propagation replaces parent values with the deepest scope instead of concatenating them; both entry points must retain the shared collection when parser trees change.
- Evidence: 1b9664cd4b; platform-suite endpoint-isolation failures and parser reproduction, 2026-10-29.

### helper-image-read-roots

- Telos: a restricted filesystem can run its sandbox helpers without exposing their neighboring files.
- Holds: helper read grants include the configured arg0 aliases and their resolved executable images, never the containing directories. The inner Linux sandbox re-execs `current_exe`, which names the image rather than the alias.
- Owners: `codex-rs/exec-server/src/fs_sandbox/helper_roots.rs`; seam in `codex-rs/exec-server/src/fs_sandbox.rs`.
- Tripwire: `helper_permissions_include_only_the_helper_executable` (`codex-rs/exec-server/src/fs_sandbox.rs`); restricted copy profiles in `codex-rs/ext/multi-env-copy-file/tests/suite/mod.rs` exercise the assembled helper chain.
- Pressure: arg0 aliases, sandbox re-exec and filesystem helper policy must agree on executable identity without granting its parent.
- Evidence: 532a6c6cf3; restricted-copy helper-path reproduction, 2026-10-29.

### fullscreen-only

- Telos: the fork maintains one frontend, the owned screen.
- Holds: no inline-renderer parity; the transcript flows from events through an owned reducer into immutable cells, and replacements go only through `App::commit_transcript_splice`; each tool history header is one compact row with source-safe disclosure; clocks and token bills sit in a fixed leading gutter; Code Mode blocks render their owned children. Human-only live work belongs below the transcript rule: current-agent operations and alarms, plus owner-labelled managed processes from its descendant tree. Disclosures determine height without an internal scrollbar or arbitrary row cap. Ctrl-click sets one semantic disclosure kind uniformly within its context: the conversation for top-level folds, the enclosing Feed Item for nested folds. That choice governs retained, newly loaded and future items until replaced; ordinary clicks remain individual exceptions. Choices are TUI-local, never persisted. Nested invocation ports follow the same item scope: Ctrl-click applies the clicked input/output visibility or Raw/Fmt choice to matching ports in that Feed Item, never the other port. Visibility preserves each port's format. A Feed Item header's port/format controls always act individually, including with Ctrl; there is no conversation-wide port expansion. Reasoning disclosures remain independent. Dialogue mode (Alt+W) is one TUI-wide lens, kept across thread switches: it hides every turn's work, including commentary, Code Mode-owned prose, running work, notices and the live tail, leaving Users, end-of-turn Responses and `/status` output stacked without blank rows, while the live work below the transcript rule stays; showing the work sets Users and Responses apart with blank rows. Detailed and raw views ignore the lens, and revealing a call shows the work. Final answers come from turn completion, never the displayed Response label. `/ps` opens this inventory rather than appending history. Liveness comes from runtime snapshots and events, not rendered transcript text; completed work leaves the live view without erasing its durable outcome.
- Owners: `AGENTS.md`, `codex-rs/tui/src/transcript_view/dialogue.rs`, `codex-rs/tui/src/app/transcript_splice.rs`, `codex-rs/tui/src/exec_cell/compact.rs`, `codex-rs/tui/src/transcript_view/text_activity.rs`, `codex-rs/tui/src/transcript_view/text_gutter.rs`, `codex-rs/tui/src/history_cell/code_mode.rs`, `codex-rs/tui/src/history_cell/code_mode_projection.rs`.
- Tripwire: `dialogue_mode_keeps_prompts_and_replies` covers dialogue admission and stacking, loaded history, live work, unanswered replies and final-answer witnesses. Gutter: `token_gutter_rounding_color_and_reflow_preserve_source` (`codex-rs/tui/src/transcript_view/text_tests.rs`), `tool_output_disclosure_shows_fixed_token_column` (`codex-rs/tui/src/transcript_view/layout_tests.rs`). Headers and Code Mode blocks: noisy (`codex-rs/tui/src/**/snapshots/*.snap`). Splice-only replacement: none.
- Pressure: 0.162 #50434 adds keyboard copy selection to the owned transcript; #50808 prunes TUI snapshots; #50109 bounds fullscreen prompts; 0.169 adds a default-off `collapse_turn_details` fold of each prompt's completed activity (`tui/src/transcript_view/turn_details.rs`, config panel, side panes), omitted by owner decision D6 (2026-11-14) because Feed Item folds and Ctrl-click disclosure own folding; `scripts/check_removed_capabilities.py` deletes `turn_details` and rejects the setting.
- Evidence: 1cf770bb6b, b78ff3aff3, eaefb734ab, ae20fc5bef, cdc2ec4845; 16e9d63de2 omits `collapse_turn_details` (owner decision D6).

### stable-feed-publication

- Telos: the feed is a factual record of state, not an engagement surface.
- Holds: required policy: eschew progressive rendering and avoid updating published items in place. Atomic units, such as injections and synchronous tool calls, are formatted and highlighted once before publication, then drawn in their final state. Open/close units publish one opening state for ongoing work and one replacement at closure, without intermediate content churn; Code Mode cells and the assistant's Responding marker follow this pattern. Assistant messages appear only when complete: a green Responding title uses the same active-work styling as code-cell titles, has no token estimates or counters, and is replaced by the prepared message block. Shimmer may indicate ongoing work; content mutation is not an animation. Ordinary redraws, viewport reflow and explicit user disclosure do not create lifecycle revisions; reuse prepared source work rather than repeating it on each paint.
- Owners: `codex-rs/tui/src/streaming/background.rs`, `codex-rs/tui/src/chatwidget/stream_preparation.rs`, `codex-rs/tui/src/history_cell/responding.rs`, `codex-rs/tui/src/app/agent_message_consolidation/continuations.rs`, `codex-rs/tui/src/app/transcript_splice.rs`, `codex-rs/tui/src/render/header.rs`, `codex-rs/tui/src/history_cell/code_mode.rs`.
- Tripwire: `controller_live_view_matches_render_during_interleaved_table_streaming` includes a fullscreen trial that withholds partial rows and preserves the complete source across width/mode changes. `list_spacing_completion_preserves_the_scrolled_reader` exercises visible response markers, out-of-order completion, interleaved content and cancellation. Feed-wide compliance outside these paths has no single check; this doctrine does not authorize an incidental migration of every existing cell.
- Pressure: upstream token-by-token streaming, provisional Markdown restyling, timer-driven source preparation and progress counters must not reintroduce content churn.
- Evidence: owner decision, 2026-10-18.

### fast-ui-thread

- Telos: typing, navigation and cancellation remain responsive while agent work, history updates and rendering preparation progress.
- Holds: required invariant: the UI dispatcher performs bounded state transitions and cached presentation; it never awaits server responses or performs unbounded I/O or source preparation. Server, configuration and filesystem work runs as typed continuations: the dispatcher admits the request, and its completion returns the outcome to the UI task. Ordering, input ownership and uncertain outcomes are preserved, unrelated editing is never held, and a request that snapshots settings takes its snapshot behind the settings barrier. Each loop pass drains a finite input batch before frame work and polls terminal input with the UI task's own waker. Relative scrolling accumulates against the requested position, not the last painted frame. Wheel speed is capped by elapsed time rather than repaint count; excess input is discarded instead of queued for later replay. An output writer thread owns the terminal: frames are latest-wins, controls and barriers keep their order, the next frame paints only after the previous one completes, and a geometry change or clear invalidates the writer's previous frame. Cell layout and completed assistant messages are prepared on background workers and published as immutable revisions; parsing, semantic formatting, highlighting and token counting finish before publication. Pending live-output preparation alone never blocks navigation or publication of ready transcript content. Width-dependent layouts are replaced only when ready; a width change blanks the transcript region rather than showing clipped rows, and the composer always renders. Unchanged source preparation is reused across headers, bodies and cosmetic ticks; animation repaints without rebuilding content. Terminals that confirm bracketed paste skip the paste-burst hold.
- Owners: `codex-rs/tui/src/app/startup.rs`, `codex-rs/tui/src/app/ui_schedule.rs`, `codex-rs/tui/src/app/server_ui_requests.rs`, `codex-rs/tui/src/app/local_ui_requests.rs`, `codex-rs/tui/src/app/navigation_config.rs`, `codex-rs/tui/src/custom_terminal/output.rs`, `codex-rs/tui/src/transcript_view/presented.rs`, `codex-rs/tui/src/streaming/background.rs`, `codex-rs/tui/src/app/owned_transcript.rs`, `codex-rs/tui/src/transcript_view/invocation.rs`, `codex-rs/tui/src/history_cell/invocation.rs`, `codex-rs/tui/src/history_cell/activity_lifecycle.rs`, `codex-rs/tui/src/transcript_view/layout.rs`, `codex-rs/tui/src/ui_latency.rs` and its endpoint, `codex-rs/cli/src/ui_latency.rs`.
- Tripwire: `blocked_writer_keeps_ui_live_and_preserves_latest_frame_and_barrier_order` stalls the writer, then checks live input, latest-frame delivery, barrier order, and repaint after width changes and clears. `controller_live_view_matches_render_during_interleaved_table_streaming` checks off-thread completion and the buffering contract in [stable-feed-publication](#stable-feed-publication). `live_preparation_does_not_freeze_retained_navigation` checks scrolling and newly committed content while a live successor is blocked; `history_prefetch_waits_for_prepared_rows` rejects unknown layout height as evidence for loading older history. `invocation_ports_preserve_sources_across_completion_and_reflow` checks preparation reuse across cosmetic ticks and unrelated call arrivals. `script_panes_settle_off_dispatcher_and_reject_stale_completions` holds the preparation worker while exercising pane controls, then checks stale-width rejection and settled live reuse. Script panes publish highlighted, wrapped copy geometry together, never a half-prepared source. `mcp_file_receipts_share_native_hunks_and_independent_disclosures` checks native projections. No in-repo latency bound: only an external pty harness checks real terminal input wake-ups and paint.
- Diagnostics: UI-task-local, fixed-stage wall-time recording publishes current work before execution. Its dedicated worker-local socket thread never depends on dispatcher/server/runtime progress; snapshots are bounded and reset is a lazy reporting epoch, not cancellation. No content, hot-path allocation, logging or disk export. Ordinary idle select is distinct from handler occupancy; timing remains inclusive, not CPU attribution. `ReactionLatency` spans input consumption to the flushed frame. Operator contract: [UI latency recorder](fork.md#ui-latency-recorder). `independent_dump_survives_blocked_and_suspended_dispatcher` and `publication_reset_race_is_bounded_and_never_accepts_torn_records` witness isolation and bounded publication/reset; they do not establish an end-to-end latency bound.
- Pressure: upstream handlers that await the server, and rendering changes that prepare on the dispatcher, reintroduce stalls; convert them to continuations or worker preparation. Crossterm's event stream latches the waker of the poll that armed its reader, so a noop-waker poll strands the UI task. A synchronous terminal query (cursor position, colors) stalls the UI until the terminal answers or the probe times out, so after startup an owned screen issues none. Only the UI task publishes current recorder work; writer-thread phases enter histograms through drained completions.
- Evidence: owner decision, 2026-10-15; responsive-ui acceptance, 2026-10-17.

### attention-gated-painting

- Telos: an unwatched TUI spends no CPU on painting, and a watched one paints only what moves.
- Holds: `PresentationMode` (`Normal`, `Peripheral`, `Reduced`, `Frozen`) follows X11 idle time and i3 visibility and focus (`DesktopAttention`); unknown visibility or focus never counts as hidden; a visible but unfocused window is `Peripheral`, where content coalesces and only the status row moves; terminal input proves a hidden report stale, and only keyboard input an unfocused one; `Reduced` and `Frozen` stop animation and title refresh; attention never suspends agent or server work; observer failure restores a usable UI. Animations share one beat (`libshimmer::TICK`), so concurrent motion shares its frames; motion answering input paints off the beat, and every animation outside the status row asks for its own frames. A full frame offers its status row when that row reaches the frame's end intact. While only that row moves, a glint repaints it over the cells beneath it and writes exactly what a full frame would for its rows (`Terminal::draw_patch`): on the beat in `Normal`, every third beat in `Peripheral`. A focused window still paints fully at least once a second; anything else owed takes a full frame (`Due`, `ScheduledPaint`).
- Owners: `codex-rs/tui/src/tui/presentation.rs` (`DesktopAttention`, `ScheduledPaint`), `codex-rs/tui/src/tui/desktop_attention.rs`, `codex-rs/tui/src/tui/frame_requester.rs` (`PresentationMode`), `codex-rs/tui/src/tui/frame_requester/beat.rs` (`Due`), `codex-rs/tui/src/tui/frame_requester/paint_schedule.rs`, `codex-rs/tui/src/tui/glint.rs`, `codex-rs/tui/src/custom_terminal/patch.rs`, `codex-rs/tui/src/app/presentation.rs`.
- Tripwire: `observer_freeze_retires_presentation_clocks_without_suspending_input` (`codex-rs/tui/src/app/tests.rs`), `reduced_presentation_keeps_status_readable_without_activity_animation` (`codex-rs/tui/src/status_indicator_widget/effects_tests.rs`), `animation_shares_the_beat_and_a_peripheral_window_keeps_only_the_glint` (`codex-rs/tui/src/tui/frame_requester.rs`), `a_patch_leaves_the_screen_a_full_frame_would` (`codex-rs/tui/src/custom_terminal/patch.rs`), `desktop_attention_recovers_on_input_and_fails_open` (`codex-rs/tui/src/tui/desktop_attention_tests.rs`).
- Pressure: none in 0.162 on the owners.
- Evidence: 5247a0733e; owner decision, 2026-10-21.

### key-doctrine

- Telos: keys mean one thing each, and none destroys work by accident.
- Holds: the default keymap binds `chat.interrupt_turn` to Ctrl+C and `chat.send_pending_steer` to Alt+I; Esc navigates and by default interrupts nothing; Ctrl+C keeps the draft; Up/Down recall only the current conversation's prompts; a mouse selection is published only to PRIMARY; copying to CLIPBOARD requires an explicit copy action. Ctrl+Shift+C and Ctrl+Insert copy the selection when forwarded by the terminal; Ctrl+O copies the last assistant response.
- Owners: `codex-rs/tui/src/app/input.rs`, `codex-rs/tui/src/text_selection.rs`, `codex-rs/tui/src/keymap.rs`, `codex-rs/tui/src/keymap/bindings.rs`, `codex-rs/config/src/tui_keymap.rs`, `codex-rs/tui/src/chatwidget/interaction.rs`, `codex-rs/tui/src/bottom_pane/chat_composer_history.rs`, `codex-rs/tui/src/transcript_view/selection.rs`, `codex-rs/tui/src/clipboard_copy.rs`.
- Tripwire: `owned_details_ctrl_c_interrupts_work_without_starting_backtrack` (`codex-rs/tui/src/app/owned_transcript_tests.rs`), `alt_i_sends_pending_steer_immediately_and_keeps_existing_draft` (`codex-rs/tui/src/chatwidget/tests/review_mode.rs`), `copy_on_select_preserves_link_activation_and_ignores_empty_drags` (`codex-rs/tui/src/transcript_view/copy_on_select_tests.rs`). Recall boundary: noisy (`conversation_prompt_recall_boundary` snapshot in `codex-rs/tui/src/chatwidget/tests/history_replay.rs`).
- Pressure: 0.162 #50389 (configured keybindings before transcript navigation), #49517 (fork shortcut) and #51500 (task pinning) edit `keymap.rs` and `tui_keymap.rs`; #50467 edits `clipboard_copy.rs`.
- Evidence: 86600d9708, d174f70315, 39fba99ad1, f36947b64a.

### saved-thread-window

- Telos: visiting a saved thread must not consume the window or draft where its link was read.
- Holds: a typed saved-thread link stays bound to its source backend. A click activates an i3 window showing the exact thread, or opens Alacritty there; it cannot switch the source TUI in place, resolve a remote ID locally, or pass the internal scheme to a browser.
- Owners: `codex-rs/tui/src/app/thread_link_handoff.rs` binds the click to its source; `codex-rs/tui/src/desktop_thread.rs` and its `windows` module own desktop handoff and presence.
- Tripwire: `linked_thread_read_uses_the_captured_connection_without_replacing_the_draft` checks source authority; `thread_mark_tracks_the_focused_terminal` checks focus and mark lifetime under isolated i3/Alacritty.
- Pressure: upstream 0.172 #62332 opens the linked thread in the current TUI.
- Evidence: owner decision, 2026-11-24.

### literal-local-paths

- Telos: a local-file reference is useful as copyable path text, without a navigation policy to maintain.
- Holds: ordinary local Markdown links preserve their labels and show their full target. Absolute targets are never shortened relative to the session cwd or frontend home. No tooltip, opener, extension allowlist, or new mouse binding is required; web links remain unchanged.
- Owners: `codex-rs/tui/src/markdown_render/local_links.rs`, `codex-rs/tui/src/markdown_render.rs`.
- Tripwire: `file_link_preserves_label_and_decodes_fallback_destination` and `file_link_keeps_unrelated_relative_label_with_matching_suffix` (`codex-rs/tui/src/markdown_render_tests.rs`).
- Pressure: upstream local-link formatting shortens targets under the session cwd; do not restore it during a merge.
- Evidence: owner decision, 2026-10-18.

### async-questions-never-auto-answered

- Telos: an agent's question is answered only by the human.
- Holds: the TUI delivers an answer only through an explicit question reply, as text the user wrote through ordinary user-turn delivery; unrelated prompts and turn endings preserve pending questions and their drafts. Opening, leaving, skipping or dismissing the question picker neither interrupts work nor synthesizes an answer.
- Owners: `codex-rs/tui/src/chatwidget/questions.rs`, `codex-rs/tui/src/async_question_reply.rs`, `codex-rs/tui/src/bottom_pane/async_questions/`, `codex-rs/tui/src/bottom_pane/request_user_input/mod.rs`.
- Tripwire: partial: `codex-rs/tui/src/chatwidget/tests/questions_tests.rs` asserts the ops that answers and follow-ups send; no test asserts that leaving or dismissing sends nothing.
- Pressure: upstream's async-question integration replaced the fork's implementation at the 0.154.0 merge (06ae488041); the retained async editor is now fork-owned, while 0.162 #51449 and #49804 edit the upstream `request_user_input/mod.rs`.
- Evidence: 193bbc9b49.

### warpaint

- Telos: one pack paints every color the TUI shows, and wearing another repaints everything, committed transcript cells included.
- Holds: a family root owns its nullable saved pack; null follows configuration. Random configuration uses stable rendezvous hashing of the canonical root thread ID, never a launch-global pick. Preview never persists. One resolver follows activation, attachment, reconnection and configuration refresh; metadata reads run independently of turn ordering, and stale replies cannot recolour another active family. No active conversation wears fixed config or `DEFAULT_PACK`. Cells hold warpaint tokens (`Color::Indexed` in reserved ranges) or upstream's named colors, never a pack color, and the binder resolves them at flush against the pack the active syntax theme was resolved from. A bound frame never meets the binder again; the editor handoff repaints it through `Terminal::replay` (`custom_terminal/output.rs`). A pack's style gives syntax classes modifiers at bind time, so style follows the worn pack as color does and never reaches a capture. No upstream code paints an extended index; `best_color` and contrast correction return RGB at 256 colors. Upstream's palette helpers still decide whether to paint, and wherever they settle on a color of their own, that role's token replaces it. Tool output is quarantined at ingestion into indices below 16 and RGB. Every painted pair is declared in `warpaint/pairs.rs` and proven by `scripts/warpaint`. The terminal's own default colors return whenever the TUI lets go of the screen.
- Owners: `codex-rs/tui/src/warpaint.rs`, `codex-rs/tui/src/warpaint/` (`paint.rs`, `pairs.rs`, `bind.rs`, `pack.rs`, `investiture.rs`, `scopes.rs`, `livery.rs`), `codex-rs/tui/src/style/speaker_backgrounds.rs`, `codex-rs/ansi-escape/src/quarantine.rs`, `codex-rs/tui/src/app/warpaint_capture.rs`, `scripts/warpaint/`, `codex-rs/tui/src/app/warpaint.rs`, `codex-rs/state/src/runtime/warpaint.rs`, `codex-rs/thread-store/src/local/warpaint.rs`, and the nullable `Thread.theme`/metadata-update protocol seams.
- Tripwire: `selected_and_resumed_threads_use_server_capability_for_v1_and_v2_children` (`codex-rs/tui/src/app/tests.rs`), `config_refresh_repaints_conversations_without_an_override` (`app/config_persistence.rs`), `daybreak_preference_survives_reconciliation_and_cold_reads` (`codex-rs/thread-store/src/local/daybreak_metadata_tests.rs`); `roles_json_matches_the_declaration` (`codex-rs/tui/src/warpaint/pairs.rs`), `embedded_packs_are_the_pack_directory` (`codex-rs/tui/src/warpaint/pack.rs`), `revoke_returns_the_colors_the_terminal_reported` (`codex-rs/tui/src/warpaint/investiture.rs`), `blocked_writer_keeps_ui_live_and_preserves_latest_frame_and_barrier_order` (`codex-rs/tui/src/custom_terminal/output.rs`) for the replay, `previewing_and_cancelling_rebinds_pack_style_over_unchanged_frames` (`codex-rs/tui/src/warpaint.rs`), `just warpaint-check`. Token painting at seams: noisy (snapshots showing `Indexed(..)`).
- Pressure: upstream palette helpers in `style.rs`, `style/contrast.rs`, `diff_render.rs`, the activity chart and the composer sparkle derive colors from the terminal's default colors. A new helper that paints its own RGB escapes the pack until it is repainted; one that paints `Color::Indexed` collides with the token ranges. `render/highlight.rs` lost two-face's bundled theme registry and custom `.tmTheme` loading: upstream edits to either conflict as modify/delete, and both stay deleted.
- Evidence: e68772b43b, 48fc0612ba, bfbe97ea8d (per-family persistence, stable rendezvous selection, non-blocking metadata reads).

## Merge hygiene

### minimal-upstream-delta

- Telos: each upstream merge conflicts only where the fork has a reason to differ.
- Holds: an upstream file differs from upstream only by seams (module declaration, call, match arm, field holding fork state); `.github/workflows/rust-ci-full-nextest-platform.yml` and `.github/workflows/rust-release.yml` equal their bytes at the merged release tag; fork instructions live in fork-owned `AGENTS.md`; a stub keeps upstream call sites unchanged.
- Owners: `AGENTS.md`.
- Tripwire: none; `git diff --quiet <release tag> HEAD -- .github/workflows/rust-ci-full-nextest-platform.yml .github/workflows/rust-release.yml` would be one.
- Pressure: every upstream merge.
- Evidence: 311bcd0f11, 69467ed11f, e4b9e0a34f, 6aa3fee801.

## Dev hygiene

### hermetic-test-runs

- Telos: a test run cannot read or disturb live Codex state.
- Holds: `just test` runs nextest through `scripts/run_tests.py`, which gives each run an empty `CODEX_HOME`, private directories bound with `bwrap` over `/run/user/<uid>/codex-1337` and `/tmp/codex-daemon-<uid>`, and disk-backed scratch that only that run deletes, and builds the binaries its selected tests spawn before running them.
- Owners: `scripts/run_tests.py`, `justfile`.
- Tripwire: scratch ownership: `test_runs_clean_only_their_own_scratch_after_exit_or_cancel` (`scripts/test_run_tests.py`, run by hand). Empty home, private runtime directories and prebuilt binaries: none.
- Pressure: none in 0.162 on the owners.
- Evidence: 1112d83c56, a365185343, dbe071b9e1, a53d7705fe.

### fork-lint-posture

- Telos: fork-owned Rust meets the house lint posture, and upstream code keeps upstream's.
- Holds: every Rust file whose path the merged release tag lacks passes `scripts/check_fork_lints.py` under `codex-rs/fork-lints.toml`. That policy is the rust-bootstrap template plus two fork exceptions, each with its reason beside it. Upstream files answer only to upstream's `[workspace.lints]`. The runner caps every lint at warn, so upstream code cannot stop the build, and fails on any diagnostic in a fork-owned file. `clippy.toml` gains `allow-panic-in-tests`; it is inert for upstream's lints.
- Owners: `codex-rs/fork-lints.toml`, `scripts/check_fork_lints.py`.
- Tripwire: the gate itself.
- Pressure: a toolchain bump can raise new findings in fork files. A file the fork moves out of an upstream path becomes fork-owned and gains the posture; an upstream release that adds a file at a fork-owned path takes that file out of the gate.
- Evidence: maintainer decision; the first run flagged 2877 findings in 444 of 592 fork-owned files.

## Index

Upstream files, where merge conflicts arise:

- `codex-rs/tui/src/markdown_render/local_links.rs`: literal-local-paths
- `codex-rs/tui/src/markdown_render.rs`: literal-local-paths, warpaint
- `codex-rs/clippy.toml`: fork-lint-posture
- `codex-rs/tui/src/app/transcript_export.rs`: fast-ui-thread
- `codex-rs/tui/src/app/thread_settings.rs`: fast-ui-thread
- `codex-rs/tui/src/app/app_server_events.rs`: fast-ui-thread
- `codex-rs/tui/src/app_server_session/history.rs`: fast-ui-thread
- `codex-rs/tui/src/app/resume_config.rs`: fast-ui-thread
- `codex-rs/tui/src/app/side.rs`: fast-ui-thread
- `codex-rs/tui/src/app/app_server_thread_ownership.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/backend_banners.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/transcript_export.rs`: fast-ui-thread
- `codex-rs/features/src/lib.rs`: append-only-tool-declarations, framed-rollouts
- `codex-rs/core/src/session/step_context.rs`: append-only-tool-declarations
- `codex-rs/core/src/session/world_state.rs`: append-only-tool-declarations, standing-context-trims, mcp-roster
- `codex-rs/Cargo.toml`: one-dependency-graph, pipelined-crate-graph, removed-stacks
- `codex-rs/app-server-client/Cargo.toml`: pipelined-crate-graph
- `codex-rs/app-server/src/request_processors/thread_queue_processor.rs`: server-hosted-agents
- `codex-rs/app-server/src/turn_admission.rs`: shared-daemon
- `codex-rs/cli/Cargo.toml`: removed-stacks
- `codex-rs/cli/src/main.rs`: pipelined-crate-graph, removed-stacks, explicit-command-verbs, managed-process-visibility, scoped-config-overrides, provider-login, framed-rollouts
- `codex-rs/exec/src/main.rs`: scoped-config-overrides
- `codex-rs/utils/cli/src/lib.rs`: scoped-config-overrides
- `codex-rs/exec-server/src/fs_sandbox.rs`: helper-image-read-roots
- `codex-rs/exec/src/cli.rs`: explicit-command-verbs
- `codex-rs/exec/src/lib.rs`: explicit-command-verbs
- `codex-rs/tui/src/cli.rs`: explicit-command-verbs
- `codex-rs/code-mode-protocol/src/grpc/codex.code_mode.v1.proto`: recoverable-output
- `codex-rs/code-mode-protocol/src/response.rs`: recoverable-output
- `codex-rs/code-mode-protocol/src/description.rs`: mcp-roster, standing-context-trims, local-coprocessor, response-order-fences
- `codex-rs/codex-mcp/src/binding.rs`: standing-context-trims
- `codex-rs/codex-mcp/src/client_tool_catalog.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/connection_manager.rs`: standing-context-trims
- `codex-rs/codex-mcp/src/connection_manager/tool_catalog.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/runtime.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/tool_catalog_cache.rs`: catalog-at-model-boundaries
- `codex-rs/codex-mcp/src/tools.rs`: catalog-at-model-boundaries
- `codex-rs/collaboration-mode-templates/templates/default.md`: standing-context-trims
- `codex-rs/config/src/tui_keymap.rs`: key-doctrine
- `codex-rs/core/src/agent/control.rs`: server-hosted-agents
- `codex-rs/core/src/compact_model_fallback.rs`: compaction-stops-on-uncertainty
- `codex-rs/core/src/compact_remote_v2.rs`: compaction-stops-on-uncertainty, compaction-is-not-input
- `codex-rs/core/src/config/mod.rs`: adapter-not-transport
- `codex-rs/core/src/context/world_state/mod.rs`: retained-goal-rules, mcp-roster
- `codex-rs/core/src/mcp_tool_call.rs`: human-only-facts, catalog-at-model-boundaries
- `codex-rs/core/src/responses_retry.rs`: backpressure-waits
- `codex-rs/core/src/session/context_window.rs`: adapter-not-transport
- `codex-rs/core/src/session/turn.rs`: native-robustness, response-order-fences
- `codex-rs/core/src/stream_events_utils.rs`: native-robustness, response-order-fences
- `codex-rs/core/src/tools/code_mode/delegate.rs`: response-order-fences
- `codex-rs/core/src/tools/code_mode/execute_handler.rs`: compact-contracts, turn-economy
- `codex-rs/core/src/tools/code_mode/execute_spec.rs`: mcp-roster
- `codex-rs/core/src/tools/code_mode/output.rs`: terse-exec-envelope, recoverable-output
- `codex-rs/core/src/tools/code_mode/response_adapter.rs`: recoverable-output
- `codex-rs/core/src/tools/code_mode/wait_handler.rs`: turn-economy
- `codex-rs/core/src/tools/context.rs`: terse-exec-envelope
- `codex-rs/core/src/tools/handlers/mcp.rs`: compact-contracts
- `codex-rs/core/src/tools/handlers/shell_spec.rs`: standing-context-trims, agent-bash
- `codex-rs/shell-command/src/agent_shell.rs`: agent-bash
- `codex-rs/code-mode-protocol/src/sh.rs`: agent-bash
- `codex-rs/core/src/session/session.rs`: agent-bash
- `codex-rs/exec-server-protocol/src/protocol.rs`: agent-bash
- `codex-rs/core/src/tools/handlers/unified_exec/exec_command.rs`: standing-context-trims, local-coprocessor
- `codex-rs/core/src/tools/parallel.rs`: response-order-fences
- `codex-rs/core/src/tools/runtimes/unified_exec.rs`: local-coprocessor
- `codex-rs/core/src/tools/spec_plan.rs`: mcp-roster, standing-context-trims, turn-economy, local-coprocessor, response-order-fences
- `codex-rs/core/src/tools/router.rs`: mcp-roster
- `codex-rs/core/src/unified_exec/process_manager.rs`: local-coprocessor, alarm-watched-sessions
- `codex-rs/ext/goal/src/extension.rs`: retained-goal-rules, goal-boxes
- `codex-rs/ext/goal/src/runtime.rs`: retained-goal-rules, goal-boxes
- `codex-rs/ext/goal/src/spec.rs`: standing-context-trims
- `codex-rs/ext/queue/src/service.rs`: server-hosted-agents
- `codex-rs/model-provider-info/src/lib.rs`: subscription-boundary, adapter-not-transport
- `codex-rs/mxc-sandbox/Cargo.toml`: removed-stacks
- `codex-rs/prompts/src/permissions_instructions.rs`: standing-context-trims
- `codex-rs/prompts/templates/permissions/approval_policy/on_request.md`: standing-context-trims
- `codex-rs/protocol/src/items.rs`: human-only-facts
- `codex-rs/protocol/src/models.rs`: native-robustness
- `codex-rs/rollout/src/policy.rs`: human-only-facts
- `codex-rs/tui/src/app/startup.rs`: fast-ui-thread, warpaint
- `codex-rs/tui/src/app/owned_transcript.rs`: fast-ui-thread, attention-gated-painting, warpaint
- `codex-rs/tui/src/transcript_view/layout.rs`: fast-ui-thread
- `codex-rs/tui/src/custom_terminal.rs`: fast-ui-thread, warpaint, attention-gated-painting
- `codex-rs/tui/src/streaming/controller.rs`: fast-ui-thread
- `codex-rs/tui/src/app_server_session.rs`: fast-ui-thread, warpaint
- `codex-rs/tui/src/app/config_persistence.rs`: fast-ui-thread, warpaint
- `codex-rs/tui/src/app/thread_goal_actions.rs`: fast-ui-thread, goal-boxes
- `codex-rs/tui/src/goal_files.rs`: fast-ui-thread
- `codex-rs/tui/src/app/pending_interactive_replay.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/thread_event_buffer.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/thread_routing.rs`: durable-approvals-and-queues, warpaint
- `codex-rs/tui/src/async_question_reply.rs`: async-questions-never-auto-answered
- `codex-rs/tui/src/bottom_pane/chat_composer_history.rs`: key-doctrine
- `codex-rs/tui/src/bottom_pane/request_user_input/mod.rs`: async-questions-never-auto-answered
- `codex-rs/tui/src/chatwidget/interaction.rs`: key-doctrine
- `codex-rs/tui/src/chatwidget.rs`: explicit-command-verbs, attention-gated-painting
- `codex-rs/tui/src/goal_display.rs`: explicit-command-verbs
- `codex-rs/tui/src/chatwidget/questions.rs`: async-questions-never-auto-answered
- `codex-rs/tui/src/clipboard_copy.rs`: key-doctrine
- `codex-rs/tui/src/exec_cell/compact.rs`: fullscreen-only
- `codex-rs/tui/src/keymap.rs`: key-doctrine
- `codex-rs/tui/src/keymap/bindings.rs`: key-doctrine
- `codex-rs/tui/src/lib.rs`: shared-daemon, warpaint, goal-boxes
- `codex-rs/tui/src/transcript_view/selection.rs`: key-doctrine
- `codex-rs/tui/src/tui/frame_requester.rs`: attention-gated-painting, fast-ui-thread
- `codex-rs/windows-sandbox-rs/Cargo.toml`: removed-stacks
- `justfile`: hermetic-test-runs, warpaint
- `codex-rs/protocol/src/tool_name.rs`: canonical-tool-identity
- `codex-rs/core/src/tools/registry.rs`: startup-tool-ceiling
- `codex-rs/tui/src/temporary_structured_request.rs`: startup-tool-ceiling
- `codex-rs/sandboxing/src/linux_pid_namespace.rs`: managed-process-visibility
- `codex-rs/core/src/compact.rs`: compaction-is-not-input
- `codex-rs/thread-store/src/local/revert_thread.rs`: accepted-agent-mail
- `codex-rs/core/src/session/handlers.rs`: accepted-agent-mail
- `codex-rs/tui/src/bottom_pane/chat_composer.rs`: client-only-shell, warpaint
- `codex-rs/app-server/src/lib.rs`: managed-process-visibility, startup-tool-ceiling, framed-rollouts
- `codex-rs/ansi-escape/src/lib.rs`: warpaint
- `codex-rs/ansi-escape/src/ansi_escape_tests.rs`: warpaint
- `codex-rs/tui/src/insert_history.rs`: warpaint
- `codex-rs/tui/src/terminal_palette.rs`: warpaint
- `codex-rs/tui/src/style.rs`: warpaint
- `codex-rs/tui/src/style/contrast.rs`: warpaint
- `codex-rs/tui/src/diff_render.rs`: warpaint
- `codex-rs/tui/src/render/highlight.rs`: warpaint
- `codex-rs/tui/src/render/mod.rs`: warpaint
- `codex-rs/tui/src/theme_picker.rs`: warpaint
- `codex-rs/tui/src/bottom_pane/mod.rs`: warpaint, attention-gated-painting
- `codex-rs/tui/src/bottom_pane/chat_composer/sparkle_field.rs`: warpaint
- `codex-rs/tui/src/analytics/activity_chart/palette.rs`: warpaint
- `codex-rs/tui/src/analytics/activity_chart/palette_tests.rs`: warpaint
- `codex-rs/tui/src/thread_color.rs` (deleted): warpaint
- `codex-rs/tui/src/thread_color_tests.rs` (deleted): warpaint
- `codex-rs/tui/src/render/model_themes.rs` (deleted): warpaint
- `codex-rs/tui/src/render/highlight_model_tests.rs` (deleted): warpaint
- `codex-rs/tui/src/theme_picker_model_tests.rs` (deleted): warpaint
- `codex-rs/tui/assets/themes/*.tmTheme` (deleted): warpaint
- `codex-rs/tui/src/app/tests/local_command_scroll_tests.rs`: warpaint
- `codex-rs/tui/src/bottom_pane/chat_composer/history_search.rs`: warpaint
- `codex-rs/tui/src/bottom_pane/chat_composer/sparkle_tests.rs`: warpaint
- `codex-rs/tui/src/bottom_pane/effort_ignition_tests.rs`: warpaint
- `codex-rs/tui/src/bottom_pane/status_line_style.rs`: warpaint
- `codex-rs/tui/src/chatwidget/rendering_tests.rs`: warpaint
- `codex-rs/tui/src/key_hint_label_tests.rs`: warpaint
- `codex-rs/tui/src/markdown_render_tests.rs`: warpaint
- `codex-rs/tui/src/resume_picker_color_tests.rs`: warpaint

- `codex-rs/tui/src/chatwidget/streaming.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view.rs`: fast-ui-thread
- `codex-rs/tui/src/terminal_probe.rs`: fast-ui-thread, warpaint
- `codex-rs/tui/src/tui/event_stream.rs`: fast-ui-thread
- `codex-rs/tui/src/tui.rs`: fast-ui-thread, warpaint, attention-gated-painting
- `codex-rs/tui/src/transcript_view/layout_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/input.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/disclosure.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/search.rs`: fast-ui-thread
- `codex-rs/tui/src/startup_draft.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/base.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/messages.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/session.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/mcp.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/plans.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/notices.rs`: fast-ui-thread
- `codex-rs/tui/src/status_indicator_widget.rs`: attention-gated-painting
- `codex-rs/tui/src/pager_overlay.rs`: attention-gated-painting
- `codex-rs/tui/src/pager_overlay/transcript.rs`: attention-gated-painting

- `codex-rs/state/src/sqlite.rs`: authoritative-queue-integrity
- `codex-rs/login/src/auth/storage.rs`: credential-backend-authority
- `codex-rs/login/src/auth/storage_telemetry.rs`: credential-backend-authority
- `codex-rs/login/src/auth/manager.rs`: pinned-accounts
- `codex-rs/codex-mcp/src/elicitation.rs`: idle-mcp-human-consent
- `codex-rs/app-server/src/outgoing_message.rs`: idle-mcp-human-consent
- `codex-rs/app-server/src/request_processors/thread_lifecycle.rs`: idle-mcp-human-consent
- `codex-rs/codex-api/src/api_bridge.rs`: native-robustness
- `codex-rs/app-server-protocol/src/protocol/v2/account.rs`: recoverable-sign-in
- `codex-rs/app-server-protocol/src/protocol/v2/shared.rs`: recoverable-sign-in
- `codex-rs/app-server/src/request_processors.rs`: goal-boxes, recoverable-sign-in
- `codex-rs/app-server/src/request_processors/thread_processor.rs`: recoverable-sign-in, warpaint
- `codex-rs/core/src/session/mod.rs`: recoverable-sign-in, recoverable-output, interleaved-turn-accounting, framed-rollouts
- `codex-rs/ext/guardian-reviewer/src/retry.rs`: recoverable-sign-in
- `codex-rs/model-provider/src/models_endpoint.rs`: recoverable-sign-in
- `codex-rs/models-manager/src/manager.rs`: recoverable-sign-in
- `codex-rs/protocol/src/codex_error_info.rs`: recoverable-sign-in
- `codex-rs/protocol/src/error.rs`: recoverable-sign-in
- `codex-rs/protocol/src/lib.rs`: goal-boxes, recoverable-sign-in
- `codex-rs/protocol/src/protocol.rs`: recoverable-sign-in
- `codex-rs/tui/src/app.rs`: warpaint, attention-gated-painting, recoverable-sign-in
- `codex-rs/tui/src/app/event_dispatch.rs`: fast-ui-thread, goal-boxes, recoverable-sign-in, warpaint
- `codex-rs/tui/src/app/reconnect.rs`: durable-approvals-and-queues, recoverable-sign-in, warpaint
- `codex-rs/tui/src/app/session_lifecycle.rs`: recoverable-sign-in
- `codex-rs/tui/src/app/thread_events.rs`: recoverable-sign-in
- `codex-rs/tui/src/app_event.rs`: goal-boxes, recoverable-sign-in, warpaint
- `codex-rs/tui/src/chatwidget/slash_dispatch.rs`: explicit-command-verbs, recoverable-sign-in
- `codex-rs/tui/src/chatwidget/turn_runtime.rs`: recoverable-sign-in
- `codex-rs/tui/src/slash_command.rs`: explicit-command-verbs, recoverable-sign-in
- `codex-rs/cli/src/login.rs`: provider-login

- `codex-rs/app-server-protocol/src/protocol/v2/tests.rs`: warpaint
- `codex-rs/app-server-protocol/src/protocol/v2/thread.rs`: warpaint
- `codex-rs/app-server-protocol/src/protocol/v2/thread_data.rs`: warpaint
- `codex-rs/app-server/src/request_processors/thread_processor_tests.rs`: warpaint
- `codex-rs/app-server/src/request_processors/thread_resume_redaction.rs`: warpaint
- `codex-rs/app-server/src/request_processors/thread_summary.rs`: warpaint
- `codex-rs/app-server/tests/suite/v2/cyber_access_program.rs`: warpaint
- `codex-rs/app-server/tests/suite/v2/mcp_resource.rs`: warpaint
- `codex-rs/app-server/tests/suite/v2/projects.rs`: warpaint
- `codex-rs/app-server/tests/suite/v2/thread_metadata_update.rs`: warpaint
- `codex-rs/app-server/tests/suite/v2/thread_resume.rs`: warpaint
- `codex-rs/app-server/tests/suite/v2/turn_start.rs`: warpaint
- `codex-rs/state/src/extract.rs`: warpaint
- `codex-rs/state/src/model/thread_metadata.rs`: warpaint
- `codex-rs/state/src/runtime.rs`: warpaint
- `codex-rs/state/src/runtime/memories.rs`: warpaint
- `codex-rs/state/src/runtime/test_support.rs`: warpaint
- `codex-rs/state/src/runtime/threads.rs`: warpaint
- `codex-rs/thread-store/src/in_memory.rs`: warpaint
- `codex-rs/thread-store/src/local/daybreak_metadata_tests.rs`: warpaint
- `codex-rs/thread-store/src/local/helpers.rs`: warpaint
- `codex-rs/thread-store/src/local/mod.rs`: warpaint, history-pruning
- `codex-rs/thread-store/src/local/read_thread.rs`: warpaint
- `codex-rs/thread-store/src/local/update_thread_metadata.rs`: warpaint
- `codex-rs/thread-store/src/types.rs`: warpaint
- `codex-rs/tui/src/app/agents_overview.rs`: warpaint
- `codex-rs/tui/src/app/agents_overview_tests.rs`: warpaint
- `codex-rs/tui/src/app/loaded_threads.rs`: warpaint
- `codex-rs/tui/src/app/test_support.rs`: warpaint
- `codex-rs/tui/src/app/tests/model_defaults_tests.rs`: warpaint
- `codex-rs/tui/src/app/tests/startup.rs`: warpaint
- `codex-rs/tui/src/app/thread_session_state.rs`: warpaint
- `codex-rs/tui/src/chatwidget/settings.rs`: warpaint
- `codex-rs/tui/src/lib_tests.rs`: warpaint
- `codex-rs/tui/src/resume_picker.rs`: warpaint

Fork-owned files, absent upstream:
- `codex-rs/tui/src/tui/frame_requester/paint_schedule.rs`: fast-ui-thread, attention-gated-painting
- `codex-rs/tui/src/tui/frame_rate_limiter/completion.rs`: fast-ui-thread
- `codex-rs/tui/src/tui/event_stream/preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/text/preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/layout_preparation_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/layout_preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/terminal_probe/imp/tests/bracketed_paste_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/streaming/controller/tests/background_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/session/snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/messages/snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/activity_presentation/live_snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/app/owned_transcript_preparation_tests.rs`: fast-ui-thread
- `codex-rs/tui/src/app/owned_transcript_input_preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/realtime_split_flap/snapshot.rs`: fast-ui-thread
- `codex-rs/tui/src/app/working_directory/admission.rs`: fast-ui-thread
- `codex-rs/tui/src/app/agents_overview_new/admission.rs`: fast-ui-thread
- `codex-rs/tui/src/app/backend_banner_fallback/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/agent_message_consolidation/continuations.rs`: fast-ui-thread, stable-feed-publication
- `codex-rs/tui/src/app/history_pagination/failed_turns.rs`: fast-ui-thread
- `codex-rs/tui/src/app/app_server_thread_ownership/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/config_persistence/continuations.rs`: fast-ui-thread, warpaint
- `codex-rs/tui/src/app/daybreak/continuations.rs`: fast-ui-thread, warpaint
- `codex-rs/tui/src/app/event_dispatch/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/experimental_features/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/model_defaults/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/test_support/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/tests/resume_shutdown_tests/verification.rs`: fast-ui-thread
- `codex-rs/tui/src/app/thread_goal_actions/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/thread_settings/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/thread_title/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app/transcript_export/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/app_server_session/typed_requests.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/backend_banners/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/rate_limits/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/session_flow/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/transcript_export/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/config_update/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/goal_files/continuations.rs`: fast-ui-thread
- `codex-rs/tui/src/test_support/server_requests.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/invocation.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/invocation.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/activity_lifecycle.rs`: fast-ui-thread
- `codex-rs/tui/src/app/ui_schedule.rs`: fast-ui-thread
- `codex-rs/tui/src/app/server_ui_requests.rs`: fast-ui-thread, warpaint
- `codex-rs/tui/src/app/local_ui_requests.rs`: fast-ui-thread
- `codex-rs/tui/src/app/navigation_config.rs`: fast-ui-thread
- `codex-rs/tui/src/app/prompt_revert.rs`: fast-ui-thread
- `codex-rs/tui/src/custom_terminal/output.rs`: fast-ui-thread, attention-gated-painting
- `codex-rs/tui/src/transcript_view/presented.rs`: fast-ui-thread
- `codex-rs/tui/src/live_work/presented.rs`: fast-ui-thread
- `codex-rs/tui/src/streaming/background.rs`: fast-ui-thread, stable-feed-publication
- `AGENTS.md`: fullscreen-only, minimal-upstream-delta
- `codex-rs/app-server-embedding/src/lib.rs`: pipelined-crate-graph
- `codex-rs/app-server-protocol/src/delegation.rs`: server-hosted-agents
- `codex-rs/app-server-protocol/src/protocol/v2/prompt_cache.rs`: cache-ttl-and-warming
- `codex-rs/app-server-protocol/src/protocol/v2/thought.rs`: token-bills
- `codex-rs/app-server/src/embedding.rs`: pipelined-crate-graph
- `codex-rs/app-server/src/prompt_cache.rs`: cache-ttl-and-warming
- `codex-rs/app-server/src/request_processors/account_processor/account_scope.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/account_processor/quota.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/account_processor/quota_cache.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/comms_gate.rs`: server-hosted-agents
- `codex-rs/app-server/src/request_processors/context_role.rs`: server-hosted-agents
- `codex-rs/app-server/src/request_processors/thread_accounts.rs`: pinned-accounts
- `codex-rs/app-server/src/request_processors/thread_warming.rs`: cache-ttl-and-warming
- `codex-rs/app-server/src/shutdown.rs`: shared-daemon
- `codex-rs/app-server/src/thread_tools/mod.rs`: server-hosted-agents
- `codex-rs/cli/src/bin/codex.rs`: pipelined-crate-graph
- `codex-rs/cli/src/reload_probe.rs`: reload-host
- `codex-rs/cli/src/sandbox_command.rs`: explicit-command-verbs
- `codex-rs/exec/src/command_grammar.rs`: explicit-command-verbs
- `codex-rs/code-mode-protocol/src/contract.rs`: compact-contracts
- `codex-rs/code-mode-protocol/src/presentation.rs`: compact-contracts, mcp-roster
- `codex-rs/code-mode-protocol/src/discovery.rs`: compact-contracts
- `codex-rs/code-mode-protocol/src/roster.rs`: mcp-roster
- `codex-rs/code-mode-runtime/src/runtime/discovery.rs`: compact-contracts
- `codex-rs/code-mode-runtime/src/runtime/filesystem.rs`: local-coprocessor
- `codex-rs/code-mode-runtime/src/runtime/encoding.rs`: local-coprocessor
- `codex-rs/code-mode-runtime/src/runtime/encoding.js`: local-coprocessor
- `codex-rs/code-mode-runtime/src/runtime/sh.rs`: recoverable-output
- `codex-rs/code-mode-runtime/src/runtime/native_result.rs`: terse-exec-envelope, recoverable-output
- `codex-rs/codex-api/src/anthropic/cache.rs`: cache-ttl-and-warming
- `codex-rs/codex-api/src/anthropic/client.rs`: backpressure-waits
- `codex-rs/codex-api/src/anthropic/decoder.rs`: native-robustness, backpressure-waits
- `codex-rs/codex-api/src/anthropic/history.rs`: reminders-not-system, stable-native-identities, messages-from-canonical-history, claude-native-voice
- `codex-rs/codex-api/src/anthropic/limits.rs`: native-robustness
- `codex-rs/codex-api/src/anthropic/mod.rs`: messages-from-canonical-history
- `codex-rs/codex-api/src/anthropic/request.rs`: claude-native-voice, subscription-boundary
- `codex-rs/codex-api/src/anthropic/salvage.rs`: messages-from-canonical-history
- `codex-rs/codex-api/src/anthropic/tool_alias.rs`: stable-native-identities
- `codex-rs/codex-api/src/anthropic/warm.rs`: cache-ttl-and-warming
- `codex-rs/config/src/anthropic_prompt_cache.rs`: cache-ttl-and-warming
- `codex-rs/core/src/agent/control/retirement.rs`: server-hosted-agents
- `codex-rs/core/src/agent/model_catalog.rs`: bare-model-slugs
- `codex-rs/core/src/backpressure.rs`: backpressure-waits
- `codex-rs/core/src/client_anthropic.rs`: subscription-boundary
- `codex-rs/core/src/client_anthropic_tools.rs`: claude-native-voice, stable-native-identities
- `codex-rs/core/src/compact_native.rs`: messages-from-canonical-history, compaction-is-not-input
- `codex-rs/core/src/compact_native_request.rs`: messages-from-canonical-history, native-robustness
- `codex-rs/core/src/config/models_cache.rs`: adapter-not-transport
- `codex-rs/core/src/context/environment_context/workspace_root_folding.rs`: standing-context-trims
- `codex-rs/core/src/context/base_instructions.rs`: instruction-amendments
- `codex-rs/core/src/context/world_state/instruction_diff.rs`: instruction-amendments
- `codex-rs/core/src/context/world_state/instruction_projection.rs`: instruction-amendments
- `codex-rs/core/src/context/world_state/nested_tools.rs`: mcp-roster
- `codex-rs/core/src/context_manager/instruction_projection.rs`: instruction-amendments
- `codex-rs/core/src/exec_policy/script_exec.rs`: local-coprocessor
- `codex-rs/core/src/guardian/reviewer_route.rs`: provider-parity
- `codex-rs/core/src/session/account.rs`: pinned-accounts
- `codex-rs/core/src/session/input_queue/interjection.rs`: turn-economy
- `codex-rs/core/src/session/response_thought.rs`: human-only-facts, token-bills
- `codex-rs/core/src/session/preamble.rs`: token-bills
- `codex-rs/core/src/session/context_inventory.rs`: token-bills
- `codex-rs/core/src/session/tool_output_tokens.rs`: token-bills
- `codex-rs/core/src/session/inject.rs`: recoverable-output
- `codex-rs/core/src/session/prepared_tool_budget.rs`: recoverable-output
- `codex-rs/core/src/tools/code_mode/output_sections.rs`: recoverable-output
- `codex-rs/core/src/tools/fence.rs`: response-order-fences
- `codex-rs/core/src/tools/router/nested_tools.rs`: mcp-roster
- `codex-rs/core/src/tools/handlers/code_mode_fs.rs`: local-coprocessor
- `codex-rs/core/src/tools/handlers/multi_agents_v2/retire_agent.rs`: server-hosted-agents
- `codex-rs/core/src/session/input_queue/mailbox_batch.rs`: turn-economy
- `codex-rs/core/src/tools/handlers/script_exec/`: local-coprocessor
- `codex-rs/core/src/tools/handlers/unified_exec/exec_command/cell.rs`: turn-economy
- `codex-rs/core/src/tools/handlers/unified_exec/write_stdin/cell.rs`: turn-economy
- `codex-rs/core/src/tools/handlers/work_status.rs`: human-only-facts
- `codex-rs/core/src/tools/script_exec.rs`: local-coprocessor
- `codex-rs/core/src/unified_exec/initial_wait.rs`: turn-economy
- `codex-rs/core/src/unified_exec/completion_receipts.rs`: turn-economy
- `codex-rs/core/src/unified_exec/output_collection.rs`: turn-economy
- `codex-rs/ext/goal/src/world_state.rs`: retained-goal-rules
- `codex-rs/feature-floor/Cargo.toml`: one-dependency-graph
- `codex-rs/history/src/tool_failure.rs`: stable-native-identities
- `codex-rs/model-provider/src/claude_catalog.rs`: claude-native-voice
- `codex-rs/protocol/src/code_mode.rs`: human-only-facts
- `codex-rs/protocol/src/comms_gate.rs`: server-hosted-agents
- `codex-rs/protocol/src/context_role.rs`: server-hosted-agents
- `codex-rs/protocol/src/mcp_file_changes.rs`: human-only-facts
- `codex-rs/protocol/src/native_execution_result.rs`: terse-exec-envelope, recoverable-output
- `codex-rs/protocol/src/preamble.rs`: human-only-facts, token-bills
- `codex-rs/state/src/runtime/farm_requests.rs`: server-hosted-agents
- `codex-rs/state/src/runtime/thread_names.rs`: server-hosted-agents
- `codex-rs/tui/src/app/idle_restart.rs`: reload-host
- `codex-rs/tui/src/app/presentation.rs`: attention-gated-painting
- `codex-rs/tui/src/app/queued_messages.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/thread_link_handoff.rs`: saved-thread-window
- `codex-rs/tui/src/desktop_thread.rs`, `codex-rs/tui/src/desktop_thread/windows.rs`: saved-thread-window
- `codex-rs/tui/src/app/transcript_splice.rs`: fullscreen-only, stable-feed-publication
- `codex-rs/tui/src/app/ui_reload.rs`: reload-host
- `codex-rs/tui/src/app_server_session/account.rs`: pinned-accounts
- `codex-rs/tui/src/chatwidget/goal_command.rs`: explicit-command-verbs
- `codex-rs/tui/src/chatwidget/context_role.rs`: explicit-command-verbs
- `codex-rs/tui/src/history_cell/code_mode.rs`: fullscreen-only, stable-feed-publication
- `codex-rs/tui/src/history_cell/code_mode_projection.rs`: fullscreen-only
- `codex-rs/tui/src/history_cell/preamble.rs`: token-bills
- `codex-rs/tui/src/history_cell/thought.rs`: token-bills
- `codex-rs/tui/src/style/speaker_backgrounds.rs`: token-bills, warpaint
- `codex-rs/tui/src/transcript_view/feed.rs`: token-bills
- `codex-rs/tui/src/reload/admission.rs`: reload-host
- `codex-rs/tui/src/reload/deadline.rs`: reload-host
- `codex-rs/tui/src/reload/host.rs`: reload-host
- `codex-rs/tui/src/reload/mod.rs`: reload-host
- `codex-rs/tui/src/reload/retained.rs`: reload-host
- `codex-rs/tui/src/transcript_view/text_activity.rs`: fullscreen-only
- `codex-rs/tui/src/transcript_view/text_gutter.rs`: token-bills, fullscreen-only
- `codex-rs/tui/src/tui/desktop_attention.rs`: attention-gated-painting
- `codex-rs/tui/src/tui/presentation.rs`: attention-gated-painting
- `codex-rs/tui/src/tui/glint.rs`: attention-gated-painting
- `codex-rs/tui/src/tui/frame_requester/beat.rs`: attention-gated-painting
- `codex-rs/tui/src/custom_terminal/patch.rs`: attention-gated-painting
- `codex-rs/tui/src/status_indicator_widget/glint.rs`: attention-gated-painting
- `codex-rs/tui/src/bottom_pane/glint.rs`: attention-gated-painting
- `codex-rs/tui/src/chatwidget/glint.rs`: attention-gated-painting
- `codex-rs/tui/src/shimmers/live.rs`: attention-gated-painting
- `codex-rs/utils/output-truncation/src/token_count.rs`: token-bills
- `docs/fork.md`: provider-parity
- `scripts/check_removed_capabilities.py`: no-terminal-pets, no-ide-context-capture, no-external-agent-import, no-cloud-connection, fullscreen-only
- `codex-rs/tui/src/prompt_request.rs`: no-ide-context-capture
- `scripts/check_feature_parity.py`: one-dependency-graph
- `scripts/run_tests.py`: pipelined-crate-graph, hermetic-test-runs
- `scripts/systemd/codex-app-server-refresh.path`: shared-daemon
- `scripts/systemd/codex-app-server-refresh.service`: shared-daemon
- `scripts/systemd/codex-app-server.service`: shared-daemon, managed-process-visibility


- `codex-rs/protocol/src/hook_selector.rs`: canonical-tool-identity
- `codex-rs/hooks/src/engine/selection.rs`: canonical-tool-identity
- `codex-rs/core/src/tool_policy.rs`: canonical-tool-identity, startup-tool-ceiling
- `codex-rs/protocol/src/tool_policy.rs`: startup-tool-ceiling



- `codex-rs/app-server/src/local_exec.rs`: managed-process-visibility


- `scripts/systemd/codex-app-server-staging.service`: managed-process-visibility
- `codex-rs/core/src/agent/control/accepted_mail.rs`: accepted-agent-mail
- `codex-rs/core/src/session/agent_mail.rs`: accepted-agent-mail
- `codex-rs/state/src/runtime/agent_mail.rs`: accepted-agent-mail
- `codex-rs/thread-store/src/live_thread/agent_mail.rs`: accepted-agent-mail
- `codex-rs/thread-store/src/local/agent_mail.rs`: accepted-agent-mail
- `codex-rs/app-server/src/request_processors/agent_mail.rs`: accepted-agent-mail
- `codex-rs/core/src/context/compaction_continuation.rs`: compaction-is-not-input


- `codex-rs/tui/src/local_shell.rs`: client-only-shell
- `codex-rs/tui/src/app/local_shell.rs`: client-only-shell
- `codex-rs/tui/src/bottom_pane/chat_composer/local_shell.rs`: client-only-shell
- `codex-rs/tui/src/chatwidget/local_shell.rs`: client-only-shell



- `codex-rs/tui/src/app/turn_submissions.rs`: durable-approvals-and-queues, warpaint
- `codex-rs/tui/src/app/turn_submission_controller.rs`: durable-approvals-and-queues, warpaint
- `codex-rs/tui/src/app/server_operation_requests.rs`: durable-approvals-and-queues, warpaint
- `codex-rs/tui/src/app/server_realtime_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_control_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_navigation_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_navigation_cleanup.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_detach_operations.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_directory_navigation.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_fork_navigation.rs`: durable-approvals-and-queues
- `codex-rs/tui/src/app/server_navigation_permissions.rs`: durable-approvals-and-queues
- `codex-rs/core/src/agent/control/farm_reservation.rs`: server-hosted-agents
- `codex-rs/tui/src/streaming/completion.rs`: fast-ui-thread
- `codex-rs/tui/src/chatwidget/stream_preparation.rs`: fast-ui-thread, stable-feed-publication
- `codex-rs/tui/src/transcript_view/layout_rewrap.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/live_preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/transcript_view/preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/tui/background_preparation.rs`: fast-ui-thread
- `codex-rs/tui/src/terminal_probe/bracketed_paste.rs`: fast-ui-thread
- `codex-rs/tui/src/history_cell/responding.rs`: stable-feed-publication
- `codex-rs/tui/src/render/header.rs`: stable-feed-publication
- `codex-rs/tui/src/warpaint.rs`: warpaint
- `codex-rs/tui/src/warpaint/`: warpaint
- `codex-rs/ansi-escape/src/quarantine.rs`: warpaint
- `codex-rs/tui/src/app/warpaint_capture.rs`: warpaint
- `scripts/warpaint/`: warpaint
- `codex-rs/app-server-protocol/src/protocol/common.rs`: goal-boxes, warpaint
- `codex-rs/app-server-protocol/src/protocol/v2/mod.rs`: goal-boxes
- `codex-rs/app-server/src/message_processor.rs`: goal-boxes
- `codex-rs/app-server/src/request_processors/account_processor.rs`: goal-boxes
- `codex-rs/app-server/src/request_processors/thread_goal_processor.rs`: goal-boxes
- `codex-rs/ext/goal/src/api.rs`: goal-boxes
- `codex-rs/ext/goal/src/lib.rs`: goal-boxes
- `codex-rs/ext/goal/src/steering.rs`: goal-boxes
- `codex-rs/state/src/runtime/goals.rs`: goal-boxes
- `codex-rs/tui/src/app/owned_transcript_tests.rs`: goal-boxes
- `codex-rs/tui/src/app/tests.rs`: goal-boxes, warpaint
- `codex-rs/tui/src/chatwidget/tests/slash_commands.rs`: goal-boxes
- `codex-rs/hooks/src/events/common.rs`: no-vacuous-injections
- `codex-rs/code-mode-protocol/src/session.rs`: code-mode-cell-outcomes
- `codex-rs/code-mode-runtime/src/session_runtime/types.rs`: code-mode-cell-outcomes
- `codex-rs/code-mode/src/remote_session/delegate.rs`: code-mode-cell-outcomes
- `codex-rs/state/src/sqlite/queue_integrity.rs`: authoritative-queue-integrity
- `codex-rs/login/src/auth/manager/identity_persistence.rs`: pinned-accounts
- `codex-rs/login/src/auth/manager/account_scope.rs`: pinned-accounts
- `codex-rs/core/src/state/token_accounting.rs`: interleaved-turn-accounting
- `codex-rs/core/src/state/session.rs`: interleaved-turn-accounting
- `codex-rs/core/src/state/mod.rs`: interleaved-turn-accounting
- `codex-rs/core/src/session/turn_context.rs`: interleaved-turn-accounting
- `codex-rs/core/src/session/mcp_elicitation_authority.rs`: idle-mcp-human-consent
- `codex-rs/codex-mcp/src/elicitation_authority.rs`: idle-mcp-human-consent
- `codex-rs/codex-mcp/src/elicitation_policy.rs`: idle-mcp-human-consent
- `codex-rs/app-server/src/idle_mcp_elicitation.rs`: idle-mcp-human-consent
- `codex-rs/core/src/unified_exec/terminal_watch.rs`: alarm-watched-sessions


- `codex-rs/app-server/src/request_processors/sign_in_required.rs`: recoverable-sign-in
- `codex-rs/core/src/session/model_catalog.rs`: recoverable-sign-in
- `codex-rs/login/src/anthropic/manager.rs`: recoverable-sign-in
- `codex-rs/model-provider/src/anthropic_auth.rs`: recoverable-sign-in
- `codex-rs/model-provider/src/native_claude_catalog.rs`: recoverable-sign-in
- `codex-rs/model-provider/src/openrouter/catalog.rs`: recoverable-sign-in
- `codex-rs/models-manager/src/authoritative_catalog.rs`: recoverable-sign-in
- `codex-rs/protocol/src/sign_in_required.rs`: recoverable-sign-in
- `codex-rs/tui/src/app/accounts.rs`: recoverable-sign-in
- `codex-rs/tui/src/app/server_thread_selection.rs`: recoverable-sign-in
- `codex-rs/tui/src/app/sign_in_required.rs`: recoverable-sign-in
- `codex-rs/tui/src/chatwidget/account_picker.rs`: recoverable-sign-in
- `codex-rs/tui/src/chatwidget/session_draft.rs`: recoverable-sign-in
- `codex-rs/tui/src/history_cell/failed_turn.rs`: recoverable-sign-in
- `codex-rs/cli/src/login_command.rs`: provider-login
- `codex-rs/cli/src/login_status.rs`: provider-login
- `codex-rs/cli/src/claude_login.rs`: provider-login

- `codex-rs/state/migrations/0069_threads_theme.sql`: warpaint
- `codex-rs/state/src/runtime/warpaint.rs`: warpaint
- `codex-rs/thread-store/src/local/warpaint.rs`: warpaint
- `codex-rs/tui/src/app/comms.rs`: warpaint
- `codex-rs/tui/src/app/thread_metadata.rs`: warpaint
- `codex-rs/tui/src/app/warpaint.rs`: warpaint
- `codex-rs/tui/src/app_server_session/fork_preparation.rs`: warpaint
- `codex-rs/rollout/src/compression.rs`: framed-rollouts
- `codex-rs/rollout/src/recorder.rs`: framed-rollouts
- `codex-rs/rollout/src/seekable_reader.rs`: framed-rollouts
- `codex-rs/rollout/src/ordinal.rs`: framed-rollouts
- `codex-rs/rollout/src/lib.rs`: framed-rollouts
- `codex-rs/rollout/src/writer_lock.rs`: framed-rollouts
- `codex-rs/thread-store/src/local/history_revision.rs`: framed-rollouts
- `codex-rs/thread-store/src/local/rollout_lineage.rs`: framed-rollouts, history-pruning
- `codex-rs/thread-store/src/local/live_writer.rs`: framed-rollouts, history-pruning
- `codex-rs/thread-store/src/local/rollout_migration/publish.rs`: framed-rollouts
- `codex-rs/core/src/thread_manager.rs`: framed-rollouts, history-pruning
- `codex-rs/app-server/src/request_processors/rollout.rs`: framed-rollouts
- `codex-rs/tui/src/resume_picker_transcript_preview.rs`: framed-rollouts
- `codex-rs/thread-store/src/local/thread_history.rs`: history-pruning
- `codex-rs/thread-store/src/local/thread_history_materialization.rs`: history-pruning
